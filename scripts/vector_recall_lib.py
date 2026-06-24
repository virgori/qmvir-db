#!/usr/bin/env python3
"""Brute-force L2 ground truth and recall@k for vector ANN benchmarks."""

from __future__ import annotations

from typing import Any, Callable


def l2_squared(query: list[float], vector: list[float]) -> float:
    return sum((a - b) * (a - b) for a, b in zip(query, vector))


def brute_force_l2_top_k(
    corpus: list[tuple[int, list[float]]],
    query: list[float],
    top_k: int,
) -> list[int]:
    scored = [(l2_squared(query, vec), row_id) for row_id, vec in corpus]
    scored.sort(key=lambda x: (x[0], x[1]))
    return [row_id for _, row_id in scored[:top_k]]


def kth_ground_truth_distance(
    corpus: list[tuple[int, list[float]]],
    query: list[float],
    top_k: int,
) -> float:
    scored = [(l2_squared(query, vec), row_id) for row_id, vec in corpus]
    scored.sort(key=lambda x: (x[0], x[1]))
    if not scored:
        return 0.0
    idx = min(top_k - 1, len(scored) - 1)
    return scored[idx][0]


def recall_at_k_by_id(predicted: list[int], ground_truth: list[int]) -> float:
    if not ground_truth:
        return 0.0
    hit = len(set(predicted) & set(ground_truth))
    return hit / len(ground_truth)


def recall_at_k_by_distance(
    predicted: list[int],
    corpus: dict[int, list[float]],
    query: list[float],
    top_k: int,
    *,
    atol: float = 1e-5,
) -> float:
    """Recall using L2^2 threshold of the exact k-th neighbor.

    Counts a hit when the predicted row's distance is within ``atol`` of the
    brute-force k-th distance. Use this when many row ids share the same vector
    (id-based recall under-counts correct ANN answers).
    """
    if top_k <= 0 or not predicted:
        return 0.0
    threshold = kth_ground_truth_distance(
        list(corpus.items()),
        query,
        top_k,
    )
    hits = 0
    for row_id in predicted[:top_k]:
        if row_id not in corpus:
            continue
        if l2_squared(query, corpus[row_id]) <= threshold + atol:
            hits += 1
    return hits / top_k


def recall_at_k(predicted: list[int], ground_truth: list[int]) -> float:
    """Backward-compatible alias for id-overlap recall."""
    return recall_at_k_by_id(predicted, ground_truth)


def parse_vector_literal(text: str) -> list[float]:
    body = text.strip()
    if body.startswith("[") and body.endswith("]"):
        body = body[1:-1]
    if not body:
        return []
    return [float(part.strip()) for part in body.split(",")]


def qm_corpus_from_table(
    qm: Any,
    *,
    table: str = "vec_bench",
    id_col: str = "id",
    embedding_col: str = "embedding",
) -> list[tuple[int, list[float]]]:
    cols, rows, _tag = qm.execute(
        f'SELECT {id_col}, {embedding_col} FROM {table} ORDER BY {id_col}'
    )
    id_idx = cols.index(id_col) if id_col in cols else 0
    emb_idx = cols.index(embedding_col) if embedding_col in cols else 1
    corpus: list[tuple[int, list[float]]] = []
    for row in rows:
        if not row or row[id_idx] is None or row[emb_idx] is None:
            continue
        raw = row[emb_idx]
        if isinstance(raw, str):
            vec = parse_vector_literal(raw)
        elif isinstance(raw, (list, tuple)):
            vec = [float(x) for x in raw]
        else:
            vec = parse_vector_literal(str(raw))
        corpus.append((int(row[id_idx]), vec))
    return corpus


def qm_select_ids(qm: Any, sql: str) -> list[int]:
    _cols, rows, _tag = qm.execute(sql)
    out: list[int] = []
    for row in rows:
        if not row or row[0] is None:
            continue
        out.append(int(row[0]))
    return out


def qdrant_point_ids(points: Any) -> list[int]:
    return [int(p.id) for p in points]


def measure_l2_recall(
    *,
    corpus: list[tuple[int, list[float]]],
    query: list[float],
    top_k: int,
    qm_ids: list[int],
    competitor_ids: list[int],
    competitor_name: str,
) -> dict[str, Any]:
    corpus_map = dict(corpus)
    ground_truth = brute_force_l2_top_k(corpus, query, top_k)
    qm_recall_id = recall_at_k_by_id(qm_ids, ground_truth)
    comp_recall_id = recall_at_k_by_id(competitor_ids, ground_truth)
    qm_recall_dist = recall_at_k_by_distance(qm_ids, corpus_map, query, top_k)
    comp_recall_dist = recall_at_k_by_distance(
        competitor_ids, corpus_map, query, top_k
    )
    slug = competitor_name.lower().replace(" ", "_")
    kth_dist = kth_ground_truth_distance(corpus, query, top_k)
    return {
        "metric": f"recall@{top_k}",
        "top_k": top_k,
        "ground_truth_method": "brute_force_l2_exact",
        "kth_ground_truth_l2_squared": round(kth_dist, 8),
        "qm": {
            "recall_by_id": round(qm_recall_id, 4),
            "recall_by_id_pct": round(100.0 * qm_recall_id, 2),
            "recall_by_distance": round(qm_recall_dist, 4),
            "recall_by_distance_pct": round(100.0 * qm_recall_dist, 2),
            # Primary marketing / gate field: distance-based (tie-aware).
            "recall": round(qm_recall_dist, 4),
            "recall_pct": round(100.0 * qm_recall_dist, 2),
            "top_k_ids_sample": qm_ids[: min(5, len(qm_ids))],
        },
        slug: {
            "recall_by_id": round(comp_recall_id, 4),
            "recall_by_id_pct": round(100.0 * comp_recall_id, 2),
            "recall_by_distance": round(comp_recall_dist, 4),
            "recall_by_distance_pct": round(100.0 * comp_recall_dist, 2),
            "recall": round(comp_recall_dist, 4),
            "recall_pct": round(100.0 * comp_recall_dist, 2),
            "top_k_ids_sample": competitor_ids[: min(5, len(competitor_ids))],
        },
        "ground_truth_sample": ground_truth[: min(5, len(ground_truth))],
        "competitor": competitor_name,
        "notes": [
            "recall_by_id: |ANN ids ∩ exact top-k ids| / k (breaks ties by row id)",
            "recall_by_distance: fraction of ANN top-k within L2^2 of exact k-th neighbor",
        ],
    }


def build_recall_suite(
    *,
    corpus: list[tuple[int, list[float]]] | None,
    query: list[float],
    qm: Any,
    query_literal: str,
    competitor_name: str,
    competitor_search: Callable[[int], list[int]],
    ks: tuple[int, ...] = (10, 50),
    table: str = "vec_bench",
) -> list[dict[str, Any]]:
    if corpus is None:
        corpus = qm_corpus_from_table(qm, table=table)
    suite: list[dict[str, Any]] = []
    for k in ks:
        qm_ids = qm_select_ids(
            qm,
            f"SELECT id FROM {table} ORDER BY embedding <-> '{query_literal}' LIMIT {k}",
        )
        comp_ids = competitor_search(k)
        suite.append(
            measure_l2_recall(
                corpus=corpus,
                query=query,
                top_k=k,
                qm_ids=qm_ids,
                competitor_ids=comp_ids,
                competitor_name=competitor_name,
            )
        )
    return suite
