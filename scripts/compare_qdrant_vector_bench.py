#!/usr/bin/env python3
"""QM vs Qdrant vector workloads — mirrors compare_postgres_vector_bench.py.

Requires: pip install qdrant-client
Env: QDRANT_URL (default http://localhost:6333)
"""

from __future__ import annotations

import argparse
import json
import os
import sys
import time
import uuid
from pathlib import Path
from typing import Any
from urllib.parse import urlparse

SCRIPT_DIR = Path(__file__).resolve().parent
sys.path.insert(0, str(SCRIPT_DIR))

from segment_benchmark_lib import (  # noqa: E402
    SegmentResult,
    bench_pair,
    bench_throughput_pair,
    print_segment_summary,
    write_segment_result,
)
from vector_recall_lib import (  # noqa: E402
    build_recall_suite,
    qdrant_point_ids,
    qm_corpus_from_table,
)


def vec_literal(i: int, dim: int) -> str:
    return "[" + ",".join(f"{((i + j) % 17) / 17.0:.6f}" for j in range(dim)) + "]"


def vec_list(i: int, dim: int) -> list[float]:
    return [((i + j) % 17) / 17.0 for j in range(dim)]


def query_literal(dim: int) -> str:
    return "[" + ",".join(f"{0.1 + j * 0.01:.6f}" for j in range(dim)) + "]"


def query_list(dim: int) -> list[float]:
    return [0.1 + j * 0.01 for j in range(dim)]


def qdrant_health(url: str) -> bool:
    try:
        import urllib.request

        parsed = urlparse(url)
        host = parsed.hostname or "localhost"
        port = parsed.port or 6333
        with urllib.request.urlopen(f"http://{host}:{port}/healthz", timeout=3) as resp:
            return resp.status == 200
    except Exception:
        return False


def recreate_collection(client: Any, qm: Any, *, name: str, dim: int, distance: str) -> None:
    if client.collection_exists(name):
        client.delete_collection(name)
    dist = (
        qm.Distance.COSINE if distance == "cosine" else qm.Distance.EUCLID
    )
    client.create_collection(
        collection_name=name,
        vectors_config=qm.VectorParams(size=dim, distance=dist),
        hnsw_config=qm.HnswConfigDiff(m=16, ef_construct=200),
    )


def setup_qdrant(
    url: str,
    *,
    collection: str,
    dim: int,
    rows: int,
    distance: str,
    client: Any | None = None,
) -> tuple[Any, Any, float]:
    from qdrant_client import QdrantClient
    from qdrant_client.http import models as qm

    if client is None:
        client = QdrantClient(url=url, timeout=120)
    recreate_collection(client, qm, name=collection, dim=dim, distance=distance)
    batch = 1000
    t0 = time.perf_counter()
    for start in range(0, rows, batch):
        end = min(rows, start + batch)
        points = [
            qm.PointStruct(id=i, vector=vec_list(i, dim), payload={"id": i})
            for i in range(start, end)
        ]
        client.upsert(collection_name=collection, points=points, wait=True)
    load_s = time.perf_counter() - t0
    return client, qm, load_s


def setup_qm(dim: int, rows: int) -> tuple[Any, float]:
    import qm_engine  # type: ignore

    qm = qm_engine.NativeSqlEngine()
    qm.execute(f"CREATE TABLE vec_bench (id INTEGER PRIMARY KEY, embedding VECTOR({dim}))")
    batch = 500
    t0 = time.perf_counter()
    for start in range(0, rows, batch):
        end = min(rows, start + batch)
        values = ",".join(
            f"({i}, '{vec_literal(i, dim)}')" for i in range(start, end)
        )
        qm.execute(f"INSERT INTO vec_bench (id, embedding) VALUES {values}")
    qm.execute("CREATE INDEX idx_vec_bench_hnsw ON vec_bench (embedding) USING hnsw")
    load_s = time.perf_counter() - t0
    return qm, load_s


def run_batch_insert_benchmark(
    client: Any,
    qm_models: Any,
    *,
    dim: int,
    rows: int,
    collection_prefix: str,
) -> dict[str, Any]:
    """Run batch insert before main corpus load so large `rows` does not skew throughput."""
    import qm_engine  # type: ignore

    batch_n = min(2000, max(200, rows // 10))
    base = 900_000
    qm_batch = qm_engine.NativeSqlEngine()
    qm_batch.execute(
        f"CREATE TABLE vec_batch (id INTEGER PRIMARY KEY, embedding VECTOR({dim}))"
    )
    values = ",".join(
        f"({base + i}, '{vec_literal(base + i, dim)}')" for i in range(batch_n)
    )
    t0 = time.perf_counter()
    qm_batch.execute(f"INSERT INTO vec_batch (id, embedding) VALUES {values}")
    qm_batch.execute("CREATE INDEX idx_vec_batch ON vec_batch (embedding) USING hnsw")
    qm_batch_rps = batch_n / (time.perf_counter() - t0)

    batch_collection = f"{collection_prefix}_batch"
    recreate_collection(
        client, qm_models, name=batch_collection, dim=dim, distance="euclid"
    )
    t0 = time.perf_counter()
    upsert_batch = 500
    for start in range(0, batch_n, upsert_batch):
        end = min(batch_n, start + upsert_batch)
        client.upsert(
            collection_name=batch_collection,
            points=[
                qm_models.PointStruct(id=base + i, vector=vec_list(base + i, dim))
                for i in range(start, end)
            ],
            wait=True,
        )
    qdrant_batch_rps = batch_n / (time.perf_counter() - t0)
    try:
        client.delete_collection(collection_name=batch_collection)
    except Exception:
        pass

    return bench_throughput_pair(
        "vector.batch_insert_multivalue",
        qm_rows_per_sec=qm_batch_rps,
        competitor_rows_per_sec=qdrant_batch_rps,
        competitor_name="Qdrant",
        rows=batch_n,
    )


def run_qdrant_vector_benchmark(
    *,
    rows: int = 100_000,
    dim: int = 32,
    iterations: int = 50,
) -> dict[str, Any]:
    try:
        from qdrant_client import QdrantClient
        from qdrant_client.http import models as qm_models
    except ImportError as exc:
        raise SystemExit("pip install qdrant-client") from exc

    url = os.environ.get("QDRANT_URL", "http://localhost:6333")
    if not qdrant_health(url):
        return {
            "error": f"Qdrant not reachable at {url}",
            "hint": "docker run -d -p 6333:6333 qdrant/qdrant",
            "rows": rows,
        }

    collection = f"qm_bench_{uuid.uuid4().hex[:8]}"
    client = QdrantClient(url=url, timeout=60)
    batch_comparison = run_batch_insert_benchmark(
        client, qm_models, dim=dim, rows=rows, collection_prefix=collection
    )
    client, qm_models, qdrant_load_s = setup_qdrant(
        url, collection=collection, dim=dim, rows=rows, distance="euclid", client=client
    )
    qm, qm_load_s = setup_qm(dim, rows)

    query_lit = query_literal(dim)
    query = query_list(dim)
    search_params = qm_models.SearchParams(hnsw_ef=40)

    next_qm_id = rows + 100_000
    next_qdrant_id = rows + 100_000
    update_qm_id = rows // 2
    update_qdrant_id = rows // 2

    result = SegmentResult(
        segment="vector",
        competitor="Qdrant",
        rows=rows,
        dim=dim,
        notes=[
            "Workloads aligned with compare_postgres_vector_bench.py",
            "Qdrant HNSW m=16 ef_construct=200; search hnsw_ef=40 (pgvector default)",
            "batch_insert runs before main corpus load (cold-process fairness)",
            f"collection={collection}",
        ],
        extra={
            "qdrant_settings": {"url": url, "hnsw_ef": 40, "m": 16},
            "setup": {
                "qm_load_elapsed_s": qm_load_s,
                "qdrant_load_elapsed_s": qdrant_load_s,
            },
        },
    )

    def qm_insert() -> None:
        nonlocal next_qm_id
        next_qm_id += 1
        lit = vec_literal(next_qm_id, dim)
        qm.execute(f"INSERT INTO vec_bench (id, embedding) VALUES ({next_qm_id}, '{lit}')")

    def qdrant_insert() -> None:
        nonlocal next_qdrant_id
        next_qdrant_id += 1
        client.upsert(
            collection_name=collection,
            points=[
                qm_models.PointStruct(
                    id=next_qdrant_id,
                    vector=vec_list(next_qdrant_id, dim),
                )
            ],
            wait=True,
        )

    result.comparison.append(
        bench_pair(
            "vector.insert_autocommit",
            max(50, iterations // 2),
            qm_insert,
            qdrant_insert,
            competitor_name="Qdrant",
        )
    )

    def qm_update() -> None:
        nonlocal update_qm_id
        update_qm_id = (update_qm_id + 1) % rows
        lit = vec_literal(update_qm_id + 7_000, dim)
        qm.execute(
            f"UPDATE vec_bench SET embedding = '{lit}' WHERE id = {update_qm_id}"
        )

    def qdrant_update() -> None:
        nonlocal update_qdrant_id
        update_qdrant_id = (update_qdrant_id + 1) % rows
        client.upsert(
            collection_name=collection,
            points=[
                qm_models.PointStruct(
                    id=update_qdrant_id,
                    vector=vec_list(update_qdrant_id + 7_000, dim),
                )
            ],
            wait=True,
        )

    result.comparison.append(
        bench_pair(
            "vector.update_autocommit",
            max(50, iterations // 2),
            qm_update,
            qdrant_update,
            competitor_name="Qdrant",
        )
    )

    def qdrant_search(limit: int) -> Any:
        return client.query_points(
            collection_name=collection,
            query=query,
            limit=limit,
            with_payload=False,
            search_params=search_params,
        ).points

    result.comparison.append(
        bench_pair(
            "vector.l2_top10_hnsw",
            iterations,
            lambda: qm.execute(
                f"SELECT id FROM vec_bench ORDER BY embedding <-> '{query_lit}' LIMIT 10"
            ),
            lambda: qdrant_search(10),
            competitor_name="Qdrant",
        )
    )
    result.comparison.append(
        bench_pair(
            "vector.l2_top50_hnsw",
            max(30, iterations // 2),
            lambda: qm.execute(
                f"SELECT id FROM vec_bench ORDER BY embedding <-> '{query_lit}' LIMIT 50"
            ),
            lambda: qdrant_search(50),
            competitor_name="Qdrant",
        )
    )

    # Cosine: QM indexed path; Qdrant uses separate short-lived cosine collection.
    cos_collection = f"{collection}_cos"
    recreate_collection(client, qm_models, name=cos_collection, dim=dim, distance="cosine")
    for start in range(0, rows, 1000):
        end = min(rows, start + 1000)
        client.upsert(
            collection_name=cos_collection,
            points=[
                qm_models.PointStruct(id=i, vector=vec_list(i, dim))
                for i in range(start, end)
            ],
            wait=True,
        )
    qm.execute(
        "CREATE INDEX idx_vec_bench_hnsw_cos ON vec_bench (embedding) USING hnsw"
    )

    result.comparison.append(
        bench_pair(
            "vector.cosine_top10_hnsw",
            max(30, iterations // 2),
            lambda: qm.execute(
                f"SELECT id FROM vec_bench ORDER BY embedding <=> '{query_lit}' LIMIT 10"
            ),
            lambda: client.query_points(
                collection_name=cos_collection,
                query=query,
                limit=10,
                with_payload=False,
                search_params=search_params,
            ).points,
            competitor_name="Qdrant",
        )
    )

    result.comparison.append(batch_comparison)

    corpus = qm_corpus_from_table(qm, table="vec_bench")
    if len(corpus) < rows:
        corpus = [(i, vec_list(i, dim)) for i in range(rows)]
    recall_suite = build_recall_suite(
        corpus=corpus,
        query=query,
        qm=qm,
        query_literal=query_lit,
        competitor_name="Qdrant",
        competitor_search=lambda k: qdrant_point_ids(qdrant_search(k)),
        ks=(10, 50),
    )
    result.extra["recall"] = recall_suite
    result.notes.append(
        "recall@k: brute-force L2 ground truth vs HNSW approximate results"
    )

    for name in (collection, cos_collection):
        try:
            client.delete_collection(collection_name=name)
        except Exception:
            pass

    return result.to_dict()


def main() -> int:
    parser = argparse.ArgumentParser(description="QM vs Qdrant vector benchmark")
    parser.add_argument("--rows", type=int, default=100_000)
    parser.add_argument("--dim", type=int, default=32)
    parser.add_argument("--iterations", type=int, default=50)
    parser.add_argument("--output", type=Path, default=Path("/tmp/qm_qdrant_vector.json"))
    args = parser.parse_args()

    payload = run_qdrant_vector_benchmark(
        rows=args.rows, dim=args.dim, iterations=args.iterations
    )
    write_segment_result(args.output, payload)
    print(json.dumps(payload, indent=2))
    if "error" not in payload:
        print()
        print_segment_summary(payload)
        for row in payload.get("recall", []):
            qm_row = row.get("qm", {})
            comp_key = row.get("competitor", "competitor").lower().replace(" ", "_")
            comp_row = row.get(comp_key, {})
            print(
                "  %-16s QM dist=%.1f%% id=%.1f%%  %s dist=%.1f%% id=%.1f%%"
                % (
                    row.get("metric", "?"),
                    qm_row.get("recall_by_distance_pct", qm_row.get("recall_pct", 0)),
                    qm_row.get("recall_by_id_pct", 0),
                    row.get("competitor", "?"),
                    comp_row.get("recall_by_distance_pct", comp_row.get("recall_pct", 0)),
                    comp_row.get("recall_by_id_pct", 0),
                )
            )
    return 0 if "error" not in payload else 1


if __name__ == "__main__":
    raise SystemExit(main())
