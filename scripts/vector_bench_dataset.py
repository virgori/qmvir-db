#!/usr/bin/env python3
"""Deterministic unique-vector corpus and query sets for ANN recall benchmarks."""

from __future__ import annotations

import random
from typing import Sequence


def vec_literal(vec: Sequence[float]) -> str:
    return "[" + ",".join(f"{float(x):.8f}" for x in vec) + "]"


def make_unique_corpus(
    *,
    rows: int,
    dim: int,
    seed: int = 42,
) -> list[tuple[int, list[float]]]:
    """Gaussian corpus — row ids 0..rows-1, vectors unique with overwhelming probability."""
    rng = random.Random(seed)
    corpus: list[tuple[int, list[float]]] = []
    for row_id in range(rows):
        vec = [rng.gauss(0.0, 1.0) for _ in range(dim)]
        corpus.append((row_id, vec))
    return corpus


def make_query_set(
    corpus: list[tuple[int, list[float]]],
    *,
    n_queries: int,
    dim: int,
    seed: int = 43,
    noise_sigma: float = 0.05,
) -> list[tuple[int, list[float]]]:
    """Queries are noisy perturbations of random corpus rows (not exact self-match)."""
    if not corpus:
        return []
    rng = random.Random(seed)
    queries: list[tuple[int, list[float]]] = []
    for query_id in range(n_queries):
        _row_id, base = rng.choice(corpus)
        q = [x + rng.gauss(0.0, noise_sigma) for x in base[:dim]]
        queries.append((query_id, q))
    return queries


def corpus_map(corpus: list[tuple[int, list[float]]]) -> dict[int, list[float]]:
    return dict(corpus)
