"""QM Vector Platform — ANN Index (Approximate Nearest Neighbor).

Supports:
  - Brute-force (exact) for small datasets
  - HNSW for fast approximate search
  - Distance metrics: cosine, dot product, L2
"""

from __future__ import annotations

import numpy as np
from dataclasses import dataclass, field
from enum import Enum
from typing import Any


class DistanceMetric(Enum):
    """Supported distance metrics."""

    COSINE = "cosine"
    DOT_PRODUCT = "dot_product"
    EUCLIDEAN = "euclidean"
    L2 = "euclidean"


@dataclass
class ANNResult:
    """Single ANN search result."""

    vector_id: str
    distance: float
    score: float  # Normalized similarity score (higher = more similar)
    metadata: dict[str, Any] = field(default_factory=dict)

    @property
    def doc_id(self) -> str:
        return self.vector_id


@dataclass
class ANNSearchResult:
    """ANN search results envelope."""

    results: list[ANNResult]
    total_searched: int
    search_time_ms: float = 0.0


def _normalize_metric(metric: DistanceMetric | str) -> DistanceMetric:
    if isinstance(metric, DistanceMetric):
        return metric
    key = str(metric).strip().lower()
    aliases = {
        "cosine": DistanceMetric.COSINE,
        "dot": DistanceMetric.DOT_PRODUCT,
        "dot_product": DistanceMetric.DOT_PRODUCT,
        "inner_product": DistanceMetric.DOT_PRODUCT,
        "ip": DistanceMetric.DOT_PRODUCT,
        "euclidean": DistanceMetric.EUCLIDEAN,
        "l2": DistanceMetric.EUCLIDEAN,
    }
    if key in aliases:
        return aliases[key]
    raise ValueError(f"Unknown distance metric: {metric}")


def _validate_dimension(dimension: int) -> int:
    if not isinstance(dimension, int) or dimension <= 0:
        raise ValueError("dimension must be a positive integer")
    return dimension


def _validate_top_k(top_k: int) -> int:
    if not isinstance(top_k, int):
        raise ValueError("top_k must be an integer")
    if top_k < 0:
        raise ValueError("top_k must be non-negative")
    return top_k


def _validate_vector(vector: np.ndarray, dimension: int, label: str) -> np.ndarray:
    arr = np.asarray(vector, dtype=np.float32)
    if arr.ndim != 1:
        raise ValueError(f"{label} vector must be one-dimensional")
    if arr.shape[0] != dimension:
        raise ValueError(
            f"{label} vector dimension mismatch: expected {dimension}, got {arr.shape[0]}"
        )
    if not np.all(np.isfinite(arr)):
        raise ValueError(f"{label} vector must contain only finite values")
    return arr


class BruteForceIndex:
    """Exact nearest neighbor search (brute force).

    Used for small datasets or as reference implementation.
    """

    def __init__(
        self,
        dimension: int,
        metric: DistanceMetric = DistanceMetric.COSINE,
    ) -> None:
        self.dimension = _validate_dimension(dimension)
        self.metric = _normalize_metric(metric)
        self._ids: list[str] = []
        self._vectors: list[np.ndarray] = []
        self._metadata: dict[str, dict[str, Any]] = {}

    def add(self, vector_id: str, vector: np.ndarray, metadata: dict[str, Any] | None = None) -> None:
        vector = _validate_vector(vector, self.dimension, "indexed")
        if vector_id in self._ids:
            idx = self._ids.index(vector_id)
            self._vectors[idx] = vector.copy()
        else:
            self._ids.append(vector_id)
            self._vectors.append(vector.copy())
        if metadata is not None:
            self._metadata[vector_id] = dict(metadata)
        else:
            self._metadata.pop(vector_id, None)

    def remove(self, vector_id: str) -> None:
        if vector_id in self._ids:
            idx = self._ids.index(vector_id)
            self._ids.pop(idx)
            self._vectors.pop(idx)
            self._metadata.pop(vector_id, None)

    def search(
        self,
        query: np.ndarray,
        top_k: int = 10,
        filter_fn: Any = None,
        k: int | None = None,
    ) -> ANNSearchResult:
        """Search for nearest neighbors."""
        import time
        if k is not None:
            top_k = k
        top_k = _validate_top_k(top_k)
        start = time.monotonic()

        query = _validate_vector(query, self.dimension, "query")
        if top_k == 0:
            return ANNSearchResult(results=[], total_searched=len(self._vectors))

        if not self._vectors:
            return ANNSearchResult(results=[], total_searched=0)

        matrix = np.stack(self._vectors)

        if self.metric == DistanceMetric.COSINE:
            distances = self._cosine_distances(query, matrix)
        elif self.metric == DistanceMetric.DOT_PRODUCT:
            distances = -np.dot(matrix, query)  # Negate for sorting
        elif self.metric == DistanceMetric.EUCLIDEAN:
            distances = np.linalg.norm(matrix - query, axis=1)
        else:
            raise ValueError(f"Unknown metric: {self.metric}")

        # Deterministic tie-break by vector id is required for release-gate tests.
        indices = sorted(range(len(distances)), key=lambda idx: (float(distances[idx]), self._ids[idx]))

        results: list[ANNResult] = []
        for idx in indices:
            if len(results) >= top_k:
                break

            vid = self._ids[idx]
            dist = float(distances[idx])

            # Apply filter if provided
            if filter_fn is not None:
                meta = self._metadata.get(vid, {})
                if not filter_fn(meta):
                    continue

            results.append(ANNResult(
                vector_id=vid,
                distance=dist,
                score=1.0 / (1.0 + dist),  # Convert distance to similarity
                metadata=self._metadata.get(vid, {}),
            ))

        elapsed = (time.monotonic() - start) * 1000
        return ANNSearchResult(
            results=results,
            total_searched=len(self._vectors),
            search_time_ms=elapsed,
        )

    def _cosine_distances(self, query: np.ndarray, matrix: np.ndarray) -> np.ndarray:
        """Compute cosine distances."""
        query_norm = query / (np.linalg.norm(query) + 1e-10)
        norms = np.linalg.norm(matrix, axis=1, keepdims=True) + 1e-10
        matrix_norm = matrix / norms
        similarities = np.dot(matrix_norm, query_norm)
        return 1.0 - similarities  # Convert similarity to distance

    @property
    def size(self) -> int:
        return len(self._ids)


class HNSWIndex:
    """HNSW (Hierarchical Navigable Small World) index.

    This is a stub that delegates to hnswlib when available,
    falling back to brute force.
    """

    def __init__(
        self,
        dimension: int,
        metric: DistanceMetric = DistanceMetric.COSINE,
        ef_construction: int = 200,
        M: int = 16,
        max_elements: int = 100_000,
    ) -> None:
        self.dimension = _validate_dimension(dimension)
        self.metric = _normalize_metric(metric)
        self.ef_construction = ef_construction
        self.M = M
        self.max_elements = max_elements
        self._fallback = BruteForceIndex(self.dimension, self.metric)
        self._hnsw = None
        self._metadata: dict[str, dict[str, Any]] = {}
        self._deleted_ids: set[int] = set()

        try:
            import hnswlib
            space = {
                DistanceMetric.COSINE: "cosine",
                DistanceMetric.DOT_PRODUCT: "ip",
                DistanceMetric.EUCLIDEAN: "l2",
            }[self.metric]
            self._hnsw = hnswlib.Index(space=space, dim=self.dimension)
            self._hnsw.init_index(
                max_elements=max_elements,
                ef_construction=ef_construction,
                M=M,
            )
            self._hnsw.set_ef(50)
            self._id_map: dict[int, str] = {}
            self._str_to_int: dict[str, int] = {}
            self._next_int_id = 0
        except ImportError:
            pass  # Use brute force fallback

    def add(self, vector_id: str, vector: np.ndarray, metadata: dict[str, Any] | None = None) -> None:
        vector = _validate_vector(vector, self.dimension, "indexed")
        self._metadata[vector_id] = dict(metadata) if metadata is not None else {}
        if self._hnsw is not None:
            if vector_id in self._str_to_int:
                int_id = self._str_to_int[vector_id]
                try:
                    self._hnsw.mark_deleted(int_id)
                    self._deleted_ids.add(int_id)
                except RuntimeError:
                    pass
            int_id = self._next_int_id
            self._next_int_id += 1
            self._id_map[int_id] = vector_id
            self._str_to_int[vector_id] = int_id
            self._deleted_ids.discard(int_id)
            self._hnsw.add_items(vector.reshape(1, -1), np.array([int_id]))
        else:
            self._fallback.add(vector_id, vector, metadata)

    def search(self, query: np.ndarray, top_k: int = 10, filter_fn: Any = None) -> ANNSearchResult:
        import time
        top_k = _validate_top_k(top_k)
        query = _validate_vector(query, self.dimension, "query")
        start = time.monotonic()
        if top_k == 0:
            return ANNSearchResult(results=[], total_searched=self.size)

        if self._hnsw is not None:
            if self.size == 0:
                return ANNSearchResult(results=[], total_searched=0)
            labels, distances = self._hnsw.knn_query(
                query.reshape(1, -1), k=min(max(top_k * 4, top_k), self.size)
            )
            results = []
            for label, dist in zip(labels[0], distances[0]):
                if len(results) >= top_k:
                    break
                if int(label) in self._deleted_ids:
                    continue
                vid = self._id_map.get(int(label), "")
                meta = self._metadata.get(vid, {})
                if filter_fn is not None and not filter_fn(meta):
                    continue
                results.append(ANNResult(
                    vector_id=vid,
                    distance=float(dist),
                    score=1.0 / (1.0 + float(dist)),
                    metadata=meta,
                ))
            elapsed = (time.monotonic() - start) * 1000
            return ANNSearchResult(results=results, total_searched=self.size, search_time_ms=elapsed)
        else:
            return self._fallback.search(query, top_k, filter_fn)

    def remove(self, vector_id: str) -> None:
        self._metadata.pop(vector_id, None)
        if self._hnsw is not None:
            int_id = self._str_to_int.pop(vector_id, None)
            if int_id is None:
                return
            self._deleted_ids.add(int_id)
            try:
                self._hnsw.mark_deleted(int_id)
            except RuntimeError:
                pass
            self._id_map.pop(int_id, None)
        else:
            self._fallback.remove(vector_id)

    @property
    def size(self) -> int:
        if self._hnsw is not None:
            return len(self._str_to_int)
        return self._fallback.size
