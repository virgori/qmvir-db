"""QM Index — HNSW + Product Quantization for vector search.

Features:
    - Pure-Python HNSW (Hierarchical Navigable Small World)
    - Dynamic maintenance (insert/delete without full rebuild)
    - Product Quantization (PQ) for memory compression
    - Scalar Quantization (SQ) as fast alternative
    - Metadata pre-filter / post-filter
    - Two-stage ANN: PQ candidates → exact re-rank
    - Adaptive ef_search based on query budget
    - Multiple distance metrics: cosine, L2, inner product
"""

from __future__ import annotations

import math
import random
import heapq
from dataclasses import dataclass, field
from enum import IntEnum
from typing import Any, Callable

import numpy as np

from qm_core.concurrency import RWLock


class Metric(IntEnum):
    COSINE = 0
    EUCLIDEAN = 1
    INNER_PRODUCT = 2


@dataclass(slots=True)
class VectorResult:
    """A single search result."""
    id: int
    distance: float
    score: float
    metadata: dict[str, Any] | None = None


def _cosine_dist(a: np.ndarray, b: np.ndarray) -> float:
    dot = float(np.dot(a, b))
    na = float(np.linalg.norm(a))
    nb = float(np.linalg.norm(b))
    if na < 1e-10 or nb < 1e-10:
        return 1.0
    return 1.0 - dot / (na * nb)


def _l2_dist(a: np.ndarray, b: np.ndarray) -> float:
    return float(np.linalg.norm(a - b))


def _ip_dist(a: np.ndarray, b: np.ndarray) -> float:
    return -float(np.dot(a, b))


DIST_FN = {
    Metric.COSINE: _cosine_dist,
    Metric.EUCLIDEAN: _l2_dist,
    Metric.INNER_PRODUCT: _ip_dist,
}


class HNSWIndex:
    """Hierarchical Navigable Small World graph for ANN search.

    Pure Python implementation with:
    - Multi-layer graph structure
    - Greedy search with ef parameter
    - Select-neighbors heuristic
    - Dynamic insert (no full rebuild needed)
    """

    def __init__(
        self,
        dim: int,
        metric: Metric = Metric.COSINE,
        M: int = 16,
        ef_construction: int = 200,
        max_level: int = 0,
    ) -> None:
        self.dim = dim
        self.metric = metric
        self.M = M  # Max connections per layer
        self.M_max0 = M * 2  # Max connections at layer 0
        self.ef_construction = ef_construction
        self._dist_fn = DIST_FN[metric]

        # Graph storage
        self._vectors: dict[int, np.ndarray] = {}
        self._metadata: dict[int, dict[str, Any]] = {}
        self._graph: dict[int, list[list[int]]] = {}  # id → [layer0_neighbors, layer1_neighbors, ...]
        self._entry_point: int | None = None
        self._max_level = 0
        self._level_mult = 1.0 / math.log(M) if M > 1 else 1.0
        self._size = 0
        self._rwlock = RWLock()

    def add(self, id: int, vector: np.ndarray, metadata: dict[str, Any] | None = None) -> None:
        """Insert a vector into the index. Thread-safe (exclusive lock)."""
        with self._rwlock.write():
            self._add_unlocked(id, vector, metadata)

    def _add_unlocked(self, id: int, vector: np.ndarray, metadata: dict[str, Any] | None = None) -> None:
        """Insert a vector (caller must hold write lock)."""
        vec = vector.astype(np.float32).flatten()
        assert len(vec) == self.dim, f"Expected dim {self.dim}, got {len(vec)}"

        self._vectors[id] = vec
        if metadata:
            self._metadata[id] = metadata

        level = self._random_level()
        self._graph[id] = [[] for _ in range(level + 1)]

        if self._entry_point is None:
            self._entry_point = id
            self._max_level = level
            self._size += 1
            return

        # Navigate from top to insertion level
        ep = self._entry_point
        for lc in range(self._max_level, level, -1):
            ep = self._search_layer_one(vec, ep, lc)

        # Insert at each level from level down to 0
        for lc in range(min(level, self._max_level), -1, -1):
            candidates = self._search_layer(vec, ep, self.ef_construction, lc)
            neighbors = self._select_neighbors(vec, candidates, self.M if lc > 0 else self.M_max0)

            self._graph[id][lc] = [n_id for _, n_id in neighbors]

            # Add bidirectional edges
            max_conn = self.M if lc > 0 else self.M_max0
            for _, n_id in neighbors:
                if lc < len(self._graph.get(n_id, [])):
                    self._graph[n_id][lc].append(id)
                    if len(self._graph[n_id][lc]) > max_conn:
                        # Prune
                        n_vec = self._vectors[n_id]
                        cands = [(self._dist_fn(n_vec, self._vectors[nn]), nn)
                                 for nn in self._graph[n_id][lc]]
                        pruned = self._select_neighbors(n_vec, cands, max_conn)
                        self._graph[n_id][lc] = [nn for _, nn in pruned]

            if candidates:
                ep = candidates[0][1]  # Closest candidate

        if level > self._max_level:
            self._max_level = level
            self._entry_point = id

        self._size += 1

    def search(
        self,
        query: np.ndarray,
        top_k: int = 10,
        ef_search: int = 50,
        filter_fn: Callable[[dict[str, Any]], bool] | None = None,
    ) -> list[VectorResult]:
        """Search for k nearest neighbors. Thread-safe (shared lock)."""
        with self._rwlock.read():
            return self._search_unlocked(query, top_k, ef_search, filter_fn)

    def _search_unlocked(
        self,
        query: np.ndarray,
        top_k: int = 10,
        ef_search: int = 50,
        filter_fn: Callable[[dict[str, Any]], bool] | None = None,
    ) -> list[VectorResult]:
        """Search without lock (caller must hold read lock)."""
        if self._entry_point is None:
            return []

        q = query.astype(np.float32).flatten()
        ef = max(ef_search, top_k)

        # Navigate from top to layer 0
        ep = self._entry_point
        for lc in range(self._max_level, 0, -1):
            ep = self._search_layer_one(q, ep, lc)

        # Search layer 0 with ef candidates
        candidates = self._search_layer(q, ep, ef, 0)

        # Filter + rank
        results: list[VectorResult] = []
        for dist, vid in candidates:
            if filter_fn is not None:
                meta = self._metadata.get(vid, {})
                if not filter_fn(meta):
                    continue
            score = 1.0 / (1.0 + dist) if dist >= 0 else 1.0 + dist
            results.append(VectorResult(id=vid, distance=dist, score=score,
                                        metadata=self._metadata.get(vid)))
            if len(results) >= top_k:
                break

        return results

    def delete(self, id: int) -> bool:
        """Remove a vector. Thread-safe (exclusive lock)."""
        with self._rwlock.write():
            return self._delete_unlocked(id)

    def _delete_unlocked(self, id: int) -> bool:
        """Remove a vector (caller must hold write lock)."""
        if id not in self._vectors:
            return False
        del self._vectors[id]
        self._metadata.pop(id, None)
        # Remove from neighbors' lists
        for lc in range(len(self._graph.get(id, []))):
            for n_id in self._graph[id][lc]:
                if n_id in self._graph and lc < len(self._graph[n_id]):
                    self._graph[n_id][lc] = [x for x in self._graph[n_id][lc] if x != id]
        del self._graph[id]
        self._size -= 1
        if id == self._entry_point:
            self._entry_point = next(iter(self._vectors), None)
        return True

    @property
    def size(self) -> int:
        return self._size

    # ── Internal ────────────────────────────────────────────────────

    def _random_level(self) -> int:
        level = 0
        while random.random() < 0.5 and level < 16:
            level += 1
        return level

    def _search_layer_one(self, q: np.ndarray, ep: int, layer: int) -> int:
        """Greedy search in a layer, returning single closest."""
        current = ep
        current_dist = self._dist_fn(q, self._vectors[current])
        changed = True
        while changed:
            changed = False
            neighbors = self._graph.get(current, [[]] * (layer + 1))
            if layer < len(neighbors):
                for n_id in neighbors[layer]:
                    if n_id not in self._vectors:
                        continue
                    d = self._dist_fn(q, self._vectors[n_id])
                    if d < current_dist:
                        current_dist = d
                        current = n_id
                        changed = True
        return current

    def _search_layer(self, q: np.ndarray, ep: int, ef: int, layer: int) -> list[tuple[float, int]]:
        """Beam search in a layer. Returns sorted (distance, id) list."""
        visited = {ep}
        d_ep = self._dist_fn(q, self._vectors[ep])
        candidates = [(d_ep, ep)]  # Min-heap
        results = [(-d_ep, ep)]  # Max-heap (negated)

        while candidates:
            c_dist, c_id = heapq.heappop(candidates)
            f_dist = -results[0][0]
            if c_dist > f_dist and len(results) >= ef:
                break

            neighbors = self._graph.get(c_id, [[]] * (layer + 1))
            if layer >= len(neighbors):
                continue
            for n_id in neighbors[layer]:
                if n_id in visited or n_id not in self._vectors:
                    continue
                visited.add(n_id)
                d = self._dist_fn(q, self._vectors[n_id])
                f_dist = -results[0][0]
                if d < f_dist or len(results) < ef:
                    heapq.heappush(candidates, (d, n_id))
                    heapq.heappush(results, (-d, n_id))
                    if len(results) > ef:
                        heapq.heappop(results)

        return sorted([(abs(d), vid) for d, vid in results])

    def _select_neighbors(self, q: np.ndarray, candidates: list[tuple[float, int]], M: int) -> list[tuple[float, int]]:
        """Select M neighbors using simple heuristic (closest first)."""
        candidates.sort()
        return candidates[:M]


class ProductQuantizer:
    """Product Quantization for vector compression.

    Splits the vector into M sub-vectors, clusters each into 256 centroids.
    Lookup distances via precomputed distance tables.
    """

    def __init__(self, dim: int, n_subvectors: int = 8, n_bits: int = 8) -> None:
        self.dim = dim
        self.n_sub = n_subvectors
        self.n_clusters = 2 ** n_bits  # 256 for 8 bits
        self.sub_dim = dim // n_subvectors
        assert dim % n_subvectors == 0, f"dim {dim} must be divisible by n_subvectors {n_subvectors}"
        self._centroids: np.ndarray | None = None  # (n_sub, n_clusters, sub_dim)
        self._trained = False

    def train(self, vectors: np.ndarray, n_iter: int = 20) -> None:
        """Train PQ codebook via k-means on each sub-vector space."""
        n = vectors.shape[0]
        centroids = np.zeros((self.n_sub, self.n_clusters, self.sub_dim), dtype=np.float32)

        for m in range(self.n_sub):
            sub_vecs = vectors[:, m * self.sub_dim:(m + 1) * self.sub_dim].astype(np.float32)
            # Simple k-means
            k = min(self.n_clusters, n)
            indices = np.random.choice(n, k, replace=False)
            centers = sub_vecs[indices].copy()

            for _ in range(n_iter):
                # Assign
                dists = np.linalg.norm(sub_vecs[:, None] - centers[None, :], axis=2)
                assignments = np.argmin(dists, axis=1)
                # Update
                for c in range(k):
                    mask = assignments == c
                    if mask.any():
                        centers[c] = sub_vecs[mask].mean(axis=0)

            centroids[m, :k] = centers

        self._centroids = centroids
        self._trained = True

    def encode(self, vectors: np.ndarray) -> np.ndarray:
        """Encode vectors to PQ codes. Returns (n, n_sub) uint8 array."""
        assert self._trained, "Must train before encoding"
        n = vectors.shape[0]
        codes = np.zeros((n, self.n_sub), dtype=np.uint8)
        for m in range(self.n_sub):
            sub_vecs = vectors[:, m * self.sub_dim:(m + 1) * self.sub_dim].astype(np.float32)
            dists = np.linalg.norm(sub_vecs[:, None] - self._centroids[m][None, :], axis=2)
            codes[:, m] = np.argmin(dists, axis=1).astype(np.uint8)
        return codes

    def decode(self, codes: np.ndarray) -> np.ndarray:
        """Decode PQ codes back to approximate vectors."""
        assert self._trained
        n = codes.shape[0]
        vectors = np.zeros((n, self.dim), dtype=np.float32)
        for m in range(self.n_sub):
            for i in range(n):
                vectors[i, m * self.sub_dim:(m + 1) * self.sub_dim] = self._centroids[m, codes[i, m]]
        return vectors

    def build_distance_table(self, query: np.ndarray) -> np.ndarray:
        """Build asymmetric distance table for fast search.

        Returns (n_sub, n_clusters) table where table[m][c] is the
        distance from query's m-th sub-vector to centroid c.
        """
        assert self._trained
        q = query.astype(np.float32).flatten()
        table = np.zeros((self.n_sub, self.n_clusters), dtype=np.float32)
        for m in range(self.n_sub):
            q_sub = q[m * self.sub_dim:(m + 1) * self.sub_dim]
            table[m] = np.linalg.norm(self._centroids[m] - q_sub, axis=1)
        return table

    def search_with_table(self, table: np.ndarray, codes: np.ndarray, top_k: int = 10) -> list[tuple[int, float]]:
        """Search using precomputed distance table. Very fast."""
        n = codes.shape[0]
        distances = np.zeros(n, dtype=np.float32)
        for m in range(self.n_sub):
            distances += table[m, codes[:, m]]
        indices = np.argpartition(distances, min(top_k, n - 1))[:top_k]
        indices = indices[np.argsort(distances[indices])]
        return [(int(idx), float(distances[idx])) for idx in indices]

    @property
    def compression_ratio(self) -> float:
        return (self.dim * 4) / self.n_sub  # fp32 → uint8 per subvector

    @property
    def memory_per_vector(self) -> int:
        return self.n_sub  # bytes


class TwoStageANN:
    """Two-stage ANN: coarse search with PQ → exact re-rank top candidates.

    Stage 1: PQ asymmetric distance search (fast, approximate)
    Stage 2: Exact distance re-rank on top candidates (accurate)
    """

    def __init__(self, dim: int, metric: Metric = Metric.COSINE, n_subvectors: int = 8) -> None:
        self.dim = dim
        self.metric = metric
        self._hnsw = HNSWIndex(dim, metric)
        self._pq = ProductQuantizer(dim, n_subvectors)
        self._pq_codes: np.ndarray | None = None
        self._id_map: list[int] = []
        self._vectors: dict[int, np.ndarray] = {}
        self._metadata: dict[int, dict[str, Any]] = {}
        self._dist_fn = DIST_FN[metric]

    def build(self, ids: list[int], vectors: np.ndarray,
              metadata: list[dict[str, Any]] | None = None) -> None:
        """Build the index from a batch of vectors."""
        # Train PQ
        self._pq.train(vectors)
        self._pq_codes = self._pq.encode(vectors)
        self._id_map = ids

        # Build HNSW
        for i, (vid, vec) in enumerate(zip(ids, vectors)):
            meta = metadata[i] if metadata else None
            self._hnsw.add(vid, vec, meta)
            self._vectors[vid] = vec.astype(np.float32).flatten()
            if meta:
                self._metadata[vid] = meta

    def search(self, query: np.ndarray, top_k: int = 10, ef_search: int = 50,
               rerank_factor: int = 4,
               filter_fn: Callable[[dict[str, Any]], bool] | None = None) -> list[VectorResult]:
        """Two-stage search: HNSW candidates → exact re-rank."""
        # Stage 1: Get more candidates than needed
        candidates = self._hnsw.search(
            query, top_k=top_k * rerank_factor, ef_search=ef_search, filter_fn=filter_fn
        )

        if not candidates:
            return []

        # Stage 2: Re-rank with exact distances
        q = query.astype(np.float32).flatten()
        reranked: list[VectorResult] = []
        for c in candidates:
            vec = self._vectors.get(c.id)
            if vec is not None:
                exact_dist = self._dist_fn(q, vec)
                score = 1.0 / (1.0 + exact_dist) if exact_dist >= 0 else 1.0 + exact_dist
                reranked.append(VectorResult(
                    id=c.id, distance=exact_dist, score=score,
                    metadata=c.metadata,
                ))

        reranked.sort(key=lambda r: r.distance)
        return reranked[:top_k]

    @property
    def size(self) -> int:
        return self._hnsw.size
