#!/usr/bin/env python3
"""Deterministic unique-vector corpus shared by QM and Qdrant ANN audits."""

from __future__ import annotations

import math
import random
from typing import Iterable


def unique_vector(row_id: int, dim: int, *, seed: int = 42) -> list[float]:
    """Deterministic, unique-ish unit-scale vector (no mod-17 degeneracy)."""
    base = (seed & 0xFFFF_FFFF) + row_id * 1_000_003
    out: list[float] = []
    for j in range(dim):
        x = (base + 1) * 0.000_137 + (j + 1) * 0.891_071
        out.append(math.sin(x) * 0.5 + math.cos(x * 1.618) * 0.5)
    return out


def vector_literal(vec: list[float]) -> str:
    return "[" + ",".join(f"{v:.8f}" for v in vec) + "]"


def build_corpus(rows: int, dim: int, *, seed: int = 42) -> list[tuple[int, list[float]]]:
    return [(i, unique_vector(i, dim, seed=seed)) for i in range(rows)]


def sample_query_ids(
    rows: int,
    n_queries: int,
    *,
    seed: int = 42,
) -> list[int]:
    rng = random.Random(seed)
    if n_queries >= rows:
        return list(range(rows))
    return rng.sample(range(rows), n_queries)


def query_vectors(
    corpus: dict[int, list[float]],
    query_ids: Iterable[int],
) -> list[tuple[int, list[float]]]:
    return [(qid, corpus[qid]) for qid in query_ids]
