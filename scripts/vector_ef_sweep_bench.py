#!/usr/bin/env python3
"""ef_search sweep: recall@k mean/p95 vs latency on unique-vector corpus."""

from __future__ import annotations

import argparse
import json
import os
import sys
import time
import uuid
from pathlib import Path
from typing import Any

SCRIPT_DIR = Path(__file__).resolve().parent
sys.path.insert(0, str(SCRIPT_DIR))

from vector_bench_dataset import make_query_set, make_unique_corpus, vec_literal  # noqa: E402
from vector_recall_bench import (  # noqa: E402
    print_summary,
    run_multi_query_recall,
    setup_qdrant_collection,
    setup_qm_table,
)


def parse_ef_list(raw: str) -> list[int]:
    return [int(x.strip()) for x in raw.split(",") if x.strip()]


def main() -> int:
    parser = argparse.ArgumentParser(description="HNSW ef_search recall sweep")
    parser.add_argument("--rows", type=int, default=10_000)
    parser.add_argument("--dim", type=int, default=32)
    parser.add_argument("--queries", type=int, default=1000)
    parser.add_argument("--seed", type=int, default=42)
    parser.add_argument("--query-seed", type=int, default=43)
    parser.add_argument("--ef", type=str, default="40,80,120,200,400")
    parser.add_argument("--ks", type=str, default="10,50,100")
    parser.add_argument("--table", type=str, default="vec_ef_sweep")
    parser.add_argument("--with-qdrant", action="store_true")
    parser.add_argument("--output", type=Path, default=Path("/tmp/qm_vector_ef_sweep.json"))
    args = parser.parse_args()

    ef_values = parse_ef_list(args.ef)
    ks = tuple(int(x.strip()) for x in args.ks.split(",") if x.strip())
    corpus = make_unique_corpus(rows=args.rows, dim=args.dim, seed=args.seed)
    queries = make_query_set(
        corpus, n_queries=args.queries, dim=args.dim, seed=args.query_seed
    )

    import qm_engine  # type: ignore

    qm = qm_engine.NativeSqlEngine()
    setup_qm_table(qm, table=args.table, dim=args.dim, corpus=corpus)

    client = None
    collection = None
    qm_models = None
    if args.with_qdrant:
        from qdrant_client import QdrantClient
        from qdrant_client.http import models as qm_models

        url = os.environ.get("QDRANT_URL", "http://localhost:6333")
        client = QdrantClient(url=url, timeout=120)
        collection = f"qm_ef_{uuid.uuid4().hex[:8]}"
        setup_qdrant_collection(
            client, qm_models, name=collection, dim=args.dim, corpus=corpus
        )

    sweep: list[dict[str, Any]] = []
    for ef in ef_values:
        latencies: list[float] = []

        def qm_search(query: list[float], top_k: int, ef_val: int) -> list[int]:
            t0 = time.perf_counter()
            ids = qm.bench_hnsw_knn_l2(args.table, "embedding", query, top_k, ef_val)
            latencies.append((time.perf_counter() - t0) * 1000.0)
            return [int(i) for i in ids]

        qdrant_fn = None
        qdrant_lat: list[float] = []
        if client is not None and collection is not None and qm_models is not None:

            def qdrant_fn(query: list[float], top_k: int, ef_val: int) -> list[int]:
                t0 = time.perf_counter()
                pts = client.query_points(
                    collection_name=collection,
                    query=query,
                    limit=top_k,
                    with_payload=False,
                    search_params=qm_models.SearchParams(hnsw_ef=ef_val),
                ).points
                qdrant_lat.append((time.perf_counter() - t0) * 1000.0)
                return [int(p.id) for p in pts]

        row = run_multi_query_recall(
            corpus=corpus,
            queries=queries,
            ks=ks,
            qm_search=qm_search,
            qdrant_search=qdrant_fn,
            ef_search=ef,
        )
        row["qm_latency_ms"] = {
            "mean": round(sum(latencies) / max(1, len(latencies)), 4),
            "p50": round(sorted(latencies)[len(latencies) // 2], 4) if latencies else 0.0,
        }
        if qdrant_fn is not None:
            row["qdrant_latency_ms"] = {
                "mean": round(sum(qdrant_lat) / max(1, len(qdrant_lat)), 4),
                "p50": round(sorted(qdrant_lat)[len(qdrant_lat) // 2], 4)
                if qdrant_lat
                else 0.0,
            }
        sweep.append(row)
        print(f"\n=== ef_search={ef} ===")
        print_summary(row)

    payload = {
        "dataset": "gaussian_unique",
        "rows": args.rows,
        "dim": args.dim,
        "queries": args.queries,
        "seed": args.seed,
        "ef_values": ef_values,
        "ks": list(ks),
        "sweep": sweep,
    }
    args.output.write_text(json.dumps(payload, indent=2))
    print(f"\nWrote {args.output}")

    if client is not None and collection is not None:
        try:
            client.delete_collection(collection_name=collection)
        except Exception:
            pass
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
