#!/usr/bin/env python3
"""ef_search sweep on unique-vector corpus — isolate graph vs tuning."""

from __future__ import annotations

import argparse
import json
import os
import statistics
import sys
import time
from pathlib import Path
from typing import Any

SCRIPT_DIR = Path(__file__).resolve().parent
sys.path.insert(0, str(SCRIPT_DIR))

from vector_bench_corpus import build_corpus, sample_query_ids, vector_literal  # noqa: E402
from vector_recall_audit import percentile, setup_qdrant, setup_qm  # noqa: E402
from vector_recall_lib import brute_force_l2_top_k, recall_at_k_by_id  # noqa: E402


def sweep_qm(
    qm: Any,
    *,
    corpus: list[tuple[int, list[float]]],
    corpus_map: dict[int, list[float]],
    query_ids: list[int],
    ef: int,
    ks: tuple[int, ...],
) -> dict[str, Any]:
    latencies_ms: list[float] = []
    recalls: dict[str, list[float]] = {f"recall_by_id@{k}": [] for k in ks}

    for qid in query_ids:
        query = corpus_map[qid]
        for k in ks:
            t0 = time.perf_counter()
            pred = list(
                qm.bench_hnsw_knn_l2("vec_audit", "embedding", query, k, ef)
            )
            latencies_ms.append((time.perf_counter() - t0) * 1000.0)
            gt = brute_force_l2_top_k(corpus, query, k)
            recalls[f"recall_by_id@{k}"].append(recall_at_k_by_id(pred, gt))

    return {
        "ef_search": ef,
        "latency_ms": {
            "mean": statistics.mean(latencies_ms) if latencies_ms else 0.0,
            "p50": percentile(latencies_ms, 50),
            "p95": percentile(latencies_ms, 95),
        },
        "recall": {
            key: {
                "mean": statistics.mean(vals) if vals else 0.0,
                "p50": percentile(vals, 50),
                "p95": percentile(vals, 95),
            }
            for key, vals in recalls.items()
        },
    }


def sweep_qdrant(
    client: Any,
    qm_models: Any,
    collection: str,
    *,
    corpus: list[tuple[int, list[float]]],
    corpus_map: dict[int, list[float]],
    query_ids: list[int],
    ef: int,
    ks: tuple[int, ...],
) -> dict[str, Any]:
    params = qm_models.SearchParams(hnsw_ef=ef)
    latencies_ms: list[float] = []
    recalls: dict[str, list[float]] = {f"recall_by_id@{k}": [] for k in ks}

    for qid in query_ids:
        query = corpus_map[qid]
        for k in ks:
            t0 = time.perf_counter()
            pts = client.query_points(
                collection_name=collection,
                query=query,
                limit=k,
                with_payload=False,
                search_params=params,
            ).points
            latencies_ms.append((time.perf_counter() - t0) * 1000.0)
            pred = [int(p.id) for p in pts]
            gt = brute_force_l2_top_k(corpus, query, k)
            recalls[f"recall_by_id@{k}"].append(recall_at_k_by_id(pred, gt))

    return {
        "ef_search": ef,
        "latency_ms": {
            "mean": statistics.mean(latencies_ms) if latencies_ms else 0.0,
            "p50": percentile(latencies_ms, 50),
            "p95": percentile(latencies_ms, 95),
        },
        "recall": {
            key: {
                "mean": statistics.mean(vals) if vals else 0.0,
                "p50": percentile(vals, 50),
                "p95": percentile(vals, 95),
            }
            for key, vals in recalls.items()
        },
    }


def run_sweep(
    *,
    rows: int,
    dim: int,
    n_queries: int,
    seed: int,
    ef_values: tuple[int, ...],
    ks: tuple[int, ...],
    qdrant_url: str | None,
) -> dict[str, Any]:
    corpus = build_corpus(rows, dim, seed=seed)
    corpus_map = dict(corpus)
    query_ids = sample_query_ids(rows, n_queries, seed=seed + 1)

    qm = setup_qm(corpus, dim)
    qm_rows: list[dict[str, Any]] = []
    for ef in ef_values:
        qm_rows.append(
            sweep_qm(
                qm,
                corpus=corpus,
                corpus_map=corpus_map,
                query_ids=query_ids,
                ef=ef,
                ks=ks,
            )
        )

    payload: dict[str, Any] = {
        "dataset": {
            "kind": "unique_deterministic",
            "rows": rows,
            "dim": dim,
            "seed": seed,
            "n_queries": n_queries,
        },
        "ks": list(ks),
        "ef_values": list(ef_values),
        "qm": qm_rows,
    }

    if qdrant_url:
        try:
            client, qm_models, collection = setup_qdrant(qdrant_url, corpus, dim)
            qd_rows: list[dict[str, Any]] = []
            for ef in ef_values:
                qd_rows.append(
                    sweep_qdrant(
                        client,
                        qm_models,
                        collection,
                        corpus=corpus,
                        corpus_map=corpus_map,
                        query_ids=query_ids,
                        ef=ef,
                        ks=ks,
                    )
                )
            payload["qdrant"] = qd_rows
            try:
                client.delete_collection(collection)
            except Exception:
                pass
        except Exception as exc:
            payload["qdrant_error"] = str(exc)

    return payload


def print_table(payload: dict[str, Any]) -> None:
    ks = payload["ks"]
    print(
        f"ef sweep rows={payload['dataset']['rows']} "
        f"queries={payload['dataset']['n_queries']}"
    )
    for engine in ("qm", "qdrant"):
        rows = payload.get(engine)
        if not rows:
            continue
        print(f"\n{engine.upper()}:")
        header = "ef".ljust(6)
        for k in ks:
            header += f"  id@{k}".ljust(12) + f"  lat_p50".ljust(10)
        print(header)
        for row in rows:
            line = str(row["ef_search"]).ljust(6)
            for k in ks:
                key = f"recall_by_id@{k}"
                mean = row["recall"][key]["mean"] * 100
                p50_lat = row["latency_ms"]["p50"]
                line += f"  {mean:5.1f}%".ljust(12) + f"  {p50_lat:6.3f}ms".ljust(10)
            print(line)


def main() -> int:
    parser = argparse.ArgumentParser(description="HNSW ef_search sweep")
    parser.add_argument("--rows", type=int, default=10_000)
    parser.add_argument("--dim", type=int, default=32)
    parser.add_argument("--queries", type=int, default=200)
    parser.add_argument("--seed", type=int, default=42)
    parser.add_argument("--ef", type=int, nargs="+", default=[40, 80, 120, 200, 400])
    parser.add_argument("--ks", type=int, nargs="+", default=[10, 50])
    parser.add_argument("--output", type=Path, default=Path("/tmp/qm_vector_ef_sweep.json"))
    parser.add_argument("--qdrant-url", default=os.environ.get("QDRANT_URL", ""))
    args = parser.parse_args()

    payload = run_sweep(
        rows=args.rows,
        dim=args.dim,
        n_queries=args.queries,
        seed=args.seed,
        ef_values=tuple(args.ef),
        ks=tuple(args.ks),
        qdrant_url=args.qdrant_url or None,
    )
    args.output.write_text(json.dumps(payload, indent=2))
    print_table(payload)
    print(f"\nWrote {args.output}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
