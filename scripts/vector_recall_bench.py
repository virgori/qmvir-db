#!/usr/bin/env python3
"""Multi-query ANN recall benchmark on unique Gaussian vectors (QM ± Qdrant)."""

from __future__ import annotations

import argparse
import json
import os
import statistics
import sys
import time
import uuid
from pathlib import Path
from typing import Any, Callable

SCRIPT_DIR = Path(__file__).resolve().parent
sys.path.insert(0, str(SCRIPT_DIR))

from vector_bench_dataset import (  # noqa: E402
    make_query_set,
    make_unique_corpus,
    vec_literal,
)
from vector_recall_lib import (  # noqa: E402
    brute_force_l2_top_k,
    qm_corpus_from_table,
    recall_at_k_by_id,
)


def percentile(values: list[float], pct: float) -> float:
    if not values:
        return 0.0
    ordered = sorted(values)
    idx = min(len(ordered) - 1, int(round((pct / 100.0) * (len(ordered) - 1))))
    return ordered[idx]


def summarize(values: list[float]) -> dict[str, float]:
    if not values:
        return {"mean": 0.0, "p50": 0.0, "p95": 0.0, "min": 0.0, "max": 0.0, "n": 0}
    return {
        "mean": round(statistics.mean(values), 4),
        "p50": round(percentile(values, 50), 4),
        "p95": round(percentile(values, 95), 4),
        "min": round(min(values), 4),
        "max": round(max(values), 4),
        "n": float(len(values)),
    }


def setup_qm_table(
    qm: Any,
    *,
    table: str,
    dim: int,
    corpus: list[tuple[int, list[float]]],
    batch: int = 500,
) -> float:
    qm.execute(f"CREATE TABLE {table} (id INTEGER PRIMARY KEY, embedding VECTOR({dim}))")
    t0 = time.perf_counter()
    for start in range(0, len(corpus), batch):
        chunk = corpus[start : start + batch]
        values = ",".join(f"({rid}, '{vec_literal(vec)}')" for rid, vec in chunk)
        qm.execute(f"INSERT INTO {table} (id, embedding) VALUES {values}")
    qm.execute(f"CREATE INDEX idx_{table}_hnsw ON {table} (embedding) USING hnsw")
    return time.perf_counter() - t0


def setup_qdrant_collection(
    client: Any,
    models: Any,
    *,
    name: str,
    dim: int,
    corpus: list[tuple[int, list[float]]],
    batch: int = 1000,
) -> float:
    if client.collection_exists(name):
        client.delete_collection(name)
    client.create_collection(
        collection_name=name,
        vectors_config=models.VectorParams(size=dim, distance=models.Distance.EUCLID),
        hnsw_config=models.HnswConfigDiff(m=16, ef_construct=200),
    )
    t0 = time.perf_counter()
    for start in range(0, len(corpus), batch):
        chunk = corpus[start : start + batch]
        client.upsert(
            collection_name=name,
            points=[
                models.PointStruct(id=rid, vector=vec) for rid, vec in chunk
            ],
            wait=True,
        )
    return time.perf_counter() - t0


def run_multi_query_recall(
    *,
    corpus: list[tuple[int, list[float]]],
    queries: list[tuple[int, list[float]]],
    ks: tuple[int, ...],
    qm_search: Callable[[list[float], int, int], list[int]],
    qdrant_search: Callable[[list[float], int, int], list[int]] | None,
    ef_search: int,
) -> dict[str, Any]:
    corpus_dict = dict(corpus)
    per_k: dict[int, dict[str, list[float]]] = {
        k: {"qm": [], "qdrant": []} for k in ks
    }

    for _qid, query in queries:
        for k in ks:
            gt = brute_force_l2_top_k(corpus, query, k)
            qm_ids = qm_search(query, k, ef_search)
            per_k[k]["qm"].append(recall_at_k_by_id(qm_ids, gt))
            if qdrant_search is not None:
                qd_ids = qdrant_search(query, k, ef_search)
                per_k[k]["qdrant"].append(recall_at_k_by_id(qd_ids, gt))

    out: dict[str, Any] = {
        "ef_search": ef_search,
        "queries": len(queries),
        "metric": "recall_by_id",
        "corpus_rows": len(corpus),
        "per_k": {},
    }
    for k in ks:
        row: dict[str, Any] = {"top_k": k, "qm": summarize(per_k[k]["qm"])}
        if qdrant_search is not None:
            row["qdrant"] = summarize(per_k[k]["qdrant"])
        out["per_k"][f"recall@{k}"] = row
    return out


def main() -> int:
    parser = argparse.ArgumentParser(description="Multi-query ANN recall benchmark")
    parser.add_argument("--rows", type=int, default=10_000)
    parser.add_argument("--dim", type=int, default=32)
    parser.add_argument("--queries", type=int, default=1000)
    parser.add_argument("--seed", type=int, default=42)
    parser.add_argument("--query-seed", type=int, default=43)
    parser.add_argument("--ef-search", type=int, default=40)
    parser.add_argument("--ks", type=str, default="10,50,100")
    parser.add_argument("--table", type=str, default="vec_recall_bench")
    parser.add_argument("--with-qdrant", action="store_true")
    parser.add_argument("--output", type=Path, default=Path("/tmp/qm_vector_recall_bench.json"))
    args = parser.parse_args()

    ks = tuple(int(x.strip()) for x in args.ks.split(",") if x.strip())
    corpus = make_unique_corpus(rows=args.rows, dim=args.dim, seed=args.seed)
    queries = make_query_set(
        corpus, n_queries=args.queries, dim=args.dim, seed=args.query_seed
    )

    import qm_engine  # type: ignore

    qm = qm_engine.NativeSqlEngine()
    qm_load_s = setup_qm_table(qm, table=args.table, dim=args.dim, corpus=corpus)
  # verify stored vectors match seed corpus
    stored = qm_corpus_from_table(qm, table=args.table)
    if len(stored) != len(corpus):
        print("WARN: stored row count mismatch", file=sys.stderr)

    def qm_search(query: list[float], top_k: int, ef: int) -> list[int]:
        ids = qm.bench_hnsw_knn_l2(args.table, "embedding", query, top_k, ef)
        return [int(i) for i in ids]

    qdrant_search_fn = None
    qdrant_load_s = None
    collection = None
    if args.with_qdrant:
        try:
            from qdrant_client import QdrantClient
            from qdrant_client.http import models as qm_models
        except ImportError:
            print("pip install qdrant-client for --with-qdrant", file=sys.stderr)
            return 1
        url = os.environ.get("QDRANT_URL", "http://localhost:6333")
        client = QdrantClient(url=url, timeout=120)
        collection = f"qm_recall_{uuid.uuid4().hex[:8]}"
        qdrant_load_s = setup_qdrant_collection(
            client, qm_models, name=collection, dim=args.dim, corpus=corpus
        )

        def qdrant_search_fn(query: list[float], top_k: int, ef: int) -> list[int]:
            pts = client.query_points(
                collection_name=collection,
                query=query,
                limit=top_k,
                with_payload=False,
                search_params=qm_models.SearchParams(hnsw_ef=ef),
            ).points
            return [int(p.id) for p in pts]

    result = run_multi_query_recall(
        corpus=corpus,
        queries=queries,
        ks=ks,
        qm_search=qm_search,
        qdrant_search=qdrant_search_fn,
        ef_search=args.ef_search,
    )
    result["setup"] = {
        "qm_load_s": round(qm_load_s, 3),
        "qdrant_load_s": round(qdrant_load_s, 3) if qdrant_load_s else None,
        "dataset": "gaussian_unique",
        "seed": args.seed,
        "query_seed": args.query_seed,
        "dim": args.dim,
        "rows": args.rows,
    }
    if collection:
        result["qdrant_collection"] = collection
        try:
            client.delete_collection(collection_name=collection)
        except Exception:
            pass

    args.output.write_text(json.dumps(result, indent=2))
    print(json.dumps(result, indent=2))
    print()
    print_summary(result)
    return 0


def print_summary(result: dict[str, Any]) -> None:
    ef = result.get("ef_search", "?")
    print(f"ef_search={ef}  queries={result.get('queries')}  metric={result.get('metric')}")
    for key, row in result.get("per_k", {}).items():
        qm = row.get("qm", {})
        line = f"  {key:12s} QM mean={qm.get('mean', 0)*100:.1f}% p95={qm.get('p95', 0)*100:.1f}%"
        if "qdrant" in row:
            qd = row["qdrant"]
            line += f"  | Qdrant mean={qd.get('mean', 0)*100:.1f}% p95={qd.get('p95', 0)*100:.1f}%"
        print(line)


if __name__ == "__main__":
    raise SystemExit(main())
