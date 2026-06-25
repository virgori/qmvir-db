#!/usr/bin/env python3
"""Multi-query ANN recall audit on unique vectors (id-based + distance-based)."""

from __future__ import annotations

import argparse
import json
import math
import os
import statistics
import sys
import time
import uuid
from pathlib import Path
from typing import Any, Callable

SCRIPT_DIR = Path(__file__).resolve().parent
sys.path.insert(0, str(SCRIPT_DIR))

from vector_bench_corpus import (  # noqa: E402
    build_corpus,
    sample_query_ids,
    vector_literal,
)
from vector_recall_lib import (  # noqa: E402
    brute_force_l2_top_k,
    recall_at_k_by_distance,
    recall_at_k_by_id,
)


def percentile(values: list[float], pct: float) -> float:
    if not values:
        return 0.0
    if len(values) == 1:
        return values[0]
    xs = sorted(values)
    rank = (pct / 100.0) * (len(xs) - 1)
    lo = int(math.floor(rank))
    hi = int(math.ceil(rank))
    if lo == hi:
        return xs[lo]
    w = rank - lo
    return xs[lo] * (1.0 - w) + xs[hi] * w


def summarize_recalls(values: list[float]) -> dict[str, float]:
    if not values:
        return {"mean": 0.0, "p50": 0.0, "p95": 0.0, "min": 0.0, "max": 0.0}
    return {
        "mean": statistics.mean(values),
        "p50": percentile(values, 50),
        "p95": percentile(values, 95),
        "min": min(values),
        "max": max(values),
    }


def setup_qm(corpus: list[tuple[int, list[float]]], dim: int) -> Any:
    import qm_engine  # type: ignore

    qm = qm_engine.NativeSqlEngine()
    qm.execute(f"CREATE TABLE vec_audit (id INTEGER PRIMARY KEY, embedding VECTOR({dim}))")
    batch = 500
    for start in range(0, len(corpus), batch):
        chunk = corpus[start : start + batch]
        values = ",".join(f"({i}, '{vector_literal(v)}')" for i, v in chunk)
        qm.execute(f"INSERT INTO vec_audit (id, embedding) VALUES {values}")
    qm.execute("CREATE INDEX idx_vec_audit_hnsw ON vec_audit (embedding) USING hnsw")
    return qm


def setup_qdrant(
    url: str,
    corpus: list[tuple[int, list[float]]],
    dim: int,
) -> tuple[Any, Any, str]:
    from qdrant_client import QdrantClient
    from qdrant_client.http import models as qm

    collection = f"vec_audit_{uuid.uuid4().hex[:8]}"
    client = QdrantClient(url=url, timeout=120)
    if client.collection_exists(collection):
        client.delete_collection(collection)
    client.create_collection(
        collection_name=collection,
        vectors_config=qm.VectorParams(size=dim, distance=qm.Distance.EUCLID),
        hnsw_config=qm.HnswConfigDiff(m=16, ef_construct=200),
    )
    batch = 1000
    for start in range(0, len(corpus), batch):
        chunk = corpus[start : start + batch]
        client.upsert(
            collection_name=collection,
            points=[
                qm.PointStruct(id=i, vector=v) for i, v in chunk
            ],
            wait=True,
        )
    return client, qm, collection


def per_query_recalls(
    *,
    corpus_map: dict[int, list[float]],
    corpus_list: list[tuple[int, list[float]]],
    query_ids: list[int],
    search_fn: Callable[[list[float], int], list[int]],
    ks: tuple[int, ...],
) -> dict[str, list[float]]:
    out: dict[str, list[float]] = {}
    for k in ks:
        out[f"recall_by_id@{k}"] = []
        out[f"recall_by_distance@{k}"] = []
    for qid in query_ids:
        query = corpus_map[qid]
        for k in ks:
            gt = brute_force_l2_top_k(corpus_list, query, k)
            pred = search_fn(query, k)
            out[f"recall_by_id@{k}"].append(recall_at_k_by_id(pred, gt))
            out[f"recall_by_distance@{k}"].append(
                recall_at_k_by_distance(pred, corpus_map, query, k)
            )
    return out


def run_audit(
    *,
    rows: int,
    dim: int,
    n_queries: int,
    seed: int,
    ks: tuple[int, ...],
    qdrant_url: str | None,
    ef_search: int,
) -> dict[str, Any]:
    corpus = build_corpus(rows, dim, seed=seed)
    corpus_map = dict(corpus)
    query_ids = sample_query_ids(rows, n_queries, seed=seed + 1)

    qm = setup_qm(corpus, dim)

    def qm_search(query: list[float], k: int) -> list[int]:
        return list(
            qm.bench_hnsw_knn_l2("vec_audit", "embedding", query, k, ef_search)
        )

    t0 = time.perf_counter()
    qm_raw = per_query_recalls(
        corpus_map=corpus_map,
        corpus_list=corpus,
        query_ids=query_ids,
        search_fn=qm_search,
        ks=ks,
    )
    qm_elapsed = time.perf_counter() - t0

    payload: dict[str, Any] = {
        "dataset": {
            "kind": "unique_deterministic",
            "rows": rows,
            "dim": dim,
            "seed": seed,
            "n_queries": n_queries,
            "query_ids_sample": query_ids[:5],
        },
        "hnsw": {"m": 16, "ef_construct": 200, "ef_search": ef_search},
        "ks": list(ks),
        "qm": {
            "search_elapsed_s": round(qm_elapsed, 4),
            "per_query": {
                key: summarize_recalls(vals) for key, vals in qm_raw.items()
            },
        },
    }

    if qdrant_url:
        try:
            client, qm_models, collection = setup_qdrant(qdrant_url, corpus, dim)
            params = qm_models.SearchParams(hnsw_ef=ef_search)

            def qdrant_search(query: list[float], k: int) -> list[int]:
                pts = client.query_points(
                    collection_name=collection,
                    query=query,
                    limit=k,
                    with_payload=False,
                    search_params=params,
                ).points
                return [int(p.id) for p in pts]

            t1 = time.perf_counter()
            qd_raw = per_query_recalls(
                corpus_map=corpus_map,
                corpus_list=corpus,
                query_ids=query_ids,
                search_fn=qdrant_search,
                ks=ks,
            )
            qd_elapsed = time.perf_counter() - t1
            payload["qdrant"] = {
                "url": qdrant_url,
                "collection": collection,
                "search_elapsed_s": round(qd_elapsed, 4),
                "per_query": {
                    key: summarize_recalls(vals) for key, vals in qd_raw.items()
                },
            }
            try:
                client.delete_collection(collection)
            except Exception:
                pass
        except Exception as exc:
            payload["qdrant_error"] = str(exc)

    return payload


def print_summary(payload: dict[str, Any]) -> None:
    ds = payload["dataset"]
    print(
        f"recall audit: rows={ds['rows']} dim={ds['dim']} "
        f"queries={ds['n_queries']} ef={payload['hnsw']['ef_search']}"
    )
    for engine in ("qm", "qdrant"):
        block = payload.get(engine)
        if not block:
            continue
        print(f"\n{engine.upper()}:")
        for k in payload["ks"]:
            for metric in ("recall_by_id", "recall_by_distance"):
                key = f"{metric}@{k}"
                stats = block["per_query"].get(key, {})
                if not stats:
                    continue
                print(
                    f"  {key:24s} mean={stats['mean']*100:5.1f}% "
                    f"p50={stats['p50']*100:5.1f}% p95={stats['p95']*100:5.1f}%"
                )
        print(f"  search_elapsed_s={block.get('search_elapsed_s')}")


def main() -> int:
    parser = argparse.ArgumentParser(description="Multi-query ANN recall audit")
    parser.add_argument("--rows", type=int, default=10_000)
    parser.add_argument("--dim", type=int, default=32)
    parser.add_argument("--queries", type=int, default=1000)
    parser.add_argument("--seed", type=int, default=42)
    parser.add_argument("--ef-search", type=int, default=40)
    parser.add_argument("--ks", type=int, nargs="+", default=[10, 50, 100])
    parser.add_argument("--output", type=Path, default=Path("/tmp/qm_vector_recall_audit.json"))
    parser.add_argument(
        "--qdrant-url",
        default=os.environ.get("QDRANT_URL", ""),
        help="Optional Qdrant URL for side-by-side audit",
    )
    args = parser.parse_args()

    payload = run_audit(
        rows=args.rows,
        dim=args.dim,
        n_queries=args.queries,
        seed=args.seed,
        ks=tuple(args.ks),
        qdrant_url=args.qdrant_url or None,
        ef_search=args.ef_search,
    )
    args.output.write_text(json.dumps(payload, indent=2))
    print_summary(payload)
    print(f"\nWrote {args.output}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
