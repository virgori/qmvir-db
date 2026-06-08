"""QM DiskANN — SSD-optimized Approximate Nearest Neighbor index.

Stores vectors on disk (SSD), keeping only the graph structure
and a compressed PQ representation in RAM.

Architecture:
    RAM:  Graph adjacency (Vamana graph) + PQ codes
    SSD:  Full-precision float32 vectors (loaded on demand for re-ranking)

Search flow:
    1. Beam search on in-memory Vamana graph using PQ distances
    2. Load top candidates' full vectors from SSD
    3. Re-rank with exact distance
    4. Return top-k

This gives HNSW-quality recall with 10-100× less RAM, at the
cost of SSD random reads during re-ranking.
"""

from __future__ import annotations

import heapq
import math
import os
import struct
from dataclasses import dataclass, field
from typing import Optional

import numpy as np


@dataclass
class DiskANNConfig:
    """DiskANN configuration."""
    dim: int = 128             # Vector dimension
    max_degree: int = 64       # Max graph edges per node (R)
    build_beam: int = 128      # Beam width during graph build (L)
    search_beam: int = 100     # Beam width during search (L_search)
    pq_subvectors: int = 16    # PQ sub-vector count (m)
    pq_centroids: int = 256    # Centroids per sub-space (k)
    alpha: float = 1.2         # Pruning parameter
    page_size: int = 4096      # SSD page alignment


class DiskANN:
    """SSD-optimized vector index using Vamana graph + PQ.

    Parameters
    ----------
    config : DiskANNConfig
        Index configuration.
    data_dir : str
        Directory for SSD vector storage.
    """

    def __init__(self, config: DiskANNConfig, data_dir: str = "/tmp/diskann"):
        self._config = config
        self._data_dir = data_dir
        os.makedirs(data_dir, exist_ok=True)

        # In-memory structures
        self._graph: dict[int, list[int]] = {}      # node → neighbors
        self._pq_codes: dict[int, np.ndarray] = {}  # node → uint8[m]
        self._medoid: int = -1                       # entry point
        self._count = 0

        # PQ codebook: (m, k, dim/m) float32
        self._pq_centroids: Optional[np.ndarray] = None
        self._sub_dim = config.dim // config.pq_subvectors

        # SSD vector file: append-only, offset-indexed
        self._vec_file_path = os.path.join(data_dir, "vectors.bin")
        self._vec_offsets: dict[int, int] = {}  # id → byte offset in file

    # ── Build ───────────────────────────────────────────────────────

    def build(self, ids: list[int], vectors: np.ndarray) -> None:
        """Build the DiskANN index from a batch of vectors.

        Parameters
        ----------
        ids : list[int]
            Vector IDs.
        vectors : np.ndarray
            (N, dim) float32 array.
        """
        n, dim = vectors.shape
        assert dim == self._config.dim

        # 1. Write all vectors to SSD
        self._write_vectors_to_disk(ids, vectors)

        # 2. Train PQ
        self._train_pq(vectors)

        # 3. Compute PQ codes for all vectors
        for i, vid in enumerate(ids):
            self._pq_codes[vid] = self._encode_pq(vectors[i])

        # 4. Build Vamana graph
        self._build_vamana(ids, vectors)

        self._count = n

    def _write_vectors_to_disk(self, ids: list[int], vectors: np.ndarray) -> None:
        """Append vectors to SSD file."""
        with open(self._vec_file_path, "wb") as f:
            for i, vid in enumerate(ids):
                offset = f.tell()
                f.write(vectors[i].astype(np.float32).tobytes())
                self._vec_offsets[vid] = offset

    def _train_pq(self, vectors: np.ndarray) -> None:
        """Train Product Quantizer codebook."""
        m = self._config.pq_subvectors
        k = self._config.pq_centroids
        sd = self._sub_dim
        n = len(vectors)

        # Initialize centroids with random selection
        centroids = np.zeros((m, k, sd), dtype=np.float32)
        for si in range(m):
            sub = vectors[:, si * sd : (si + 1) * sd]
            # Simple k-means (few iterations for speed)
            indices = np.random.choice(n, min(k, n), replace=False)
            centroids[si, :min(k, n)] = sub[indices]
            for _ in range(5):  # 5 k-means iterations
                # Assign
                dists = np.sum(
                    (sub[:, None, :] - centroids[si, None, :min(k, n), :]) ** 2,
                    axis=2,
                )
                assignments = np.argmin(dists, axis=1)
                # Update
                for ci in range(min(k, n)):
                    mask = assignments == ci
                    if mask.any():
                        centroids[si, ci] = sub[mask].mean(axis=0)

        self._pq_centroids = centroids

    def _encode_pq(self, vector: np.ndarray) -> np.ndarray:
        """Encode a vector to PQ codes (uint8[m])."""
        m = self._config.pq_subvectors
        sd = self._sub_dim
        codes = np.zeros(m, dtype=np.uint8)
        for si in range(m):
            sub = vector[si * sd : (si + 1) * sd]
            dists = np.sum((self._pq_centroids[si] - sub) ** 2, axis=1)
            codes[si] = np.argmin(dists)
        return codes

    def _build_pq_lookup_table(self, query: np.ndarray) -> np.ndarray:
        """Pre-compute PQ distance lookup table for a query.

        Returns (m, k) float32 array where entry [si, ci] is the squared
        distance from query sub-vector si to centroid ci.
        This turns per-candidate distance from O(dim) to O(m) table lookups.
        """
        m = self._config.pq_subvectors
        k = self._config.pq_centroids
        sd = self._sub_dim
        table = np.zeros((m, k), dtype=np.float32)
        for si in range(m):
            sub = query[si * sd : (si + 1) * sd]
            table[si] = np.sum((self._pq_centroids[si] - sub) ** 2, axis=1)
        return table

    def _pq_distance_lut(self, lut: np.ndarray, codes: np.ndarray) -> float:
        """Fast PQ distance using pre-computed lookup table."""
        dist = 0.0
        for si in range(len(codes)):
            dist += float(lut[si, codes[si]])
        return dist

    def _pq_distance(self, query: np.ndarray, codes: np.ndarray) -> float:
        """Compute approximate distance using PQ (no LUT)."""
        m = self._config.pq_subvectors
        sd = self._sub_dim
        dist = 0.0
        for si in range(m):
            sub = query[si * sd : (si + 1) * sd]
            centroid = self._pq_centroids[si, codes[si]]
            dist += float(np.sum((sub - centroid) ** 2))
        return dist

    def _build_vamana(self, ids: list[int], vectors: np.ndarray) -> None:
        """Build Vamana graph (simplified greedy construction)."""
        n = len(ids)
        R = self._config.max_degree
        L = self._config.build_beam

        # Initialize empty graph
        for vid in ids:
            self._graph[vid] = []

        # Set medoid as closest to centroid
        centroid = vectors.mean(axis=0)
        dists_to_centroid = np.sum((vectors - centroid) ** 2, axis=1)
        self._medoid = ids[int(np.argmin(dists_to_centroid))]

        # Build map for quick lookup during construction
        id_to_idx = {vid: i for i, vid in enumerate(ids)}

        # Greedy insertion
        for idx, vid in enumerate(ids):
            if idx == 0:
                continue

            # Build subset mapping for nodes inserted so far
            sub_ids = ids[:idx + 1]
            sub_vectors = vectors[:idx + 1]
            sub_id_to_idx = {v: i for i, v in enumerate(sub_ids)}

            # Greedy search from medoid to find neighbors
            candidates = self._greedy_search_build(
                vectors[idx], sub_ids, sub_vectors,
                sub_id_to_idx, L,
            )

            # Robust prune: keep R closest with diversity
            neighbors = self._robust_prune(
                vid, candidates, sub_ids, sub_vectors, sub_id_to_idx, R,
            )
            self._graph[vid] = neighbors

            # Add reverse edges
            for nb in neighbors:
                if vid not in self._graph[nb]:
                    if len(self._graph[nb]) < R:
                        self._graph[nb].append(vid)

    def _greedy_search_build(
        self,
        query: np.ndarray,
        ids: list[int],
        vectors: np.ndarray,
        id_to_idx: dict[int, int],
        beam: int,
    ) -> list[tuple[float, int]]:
        """Greedy search for graph construction."""
        visited: set[int] = set()
        # Start from medoid
        start = self._medoid if self._medoid in id_to_idx else ids[0]
        start_idx = id_to_idx[start]
        d = float(np.sum((query - vectors[start_idx]) ** 2))
        heap: list[tuple[float, int]] = [(d, start)]
        visited.add(start)
        results: list[tuple[float, int]] = [(d, start)]

        while heap:
            dist, current = heapq.heappop(heap)
            for nb in self._graph.get(current, []):
                if nb not in visited and nb in id_to_idx:
                    visited.add(nb)
                    nb_idx = id_to_idx[nb]
                    nd = float(np.sum((query - vectors[nb_idx]) ** 2))
                    heapq.heappush(heap, (nd, nb))
                    results.append((nd, nb))

            if len(visited) >= beam:
                break

        results.sort()
        return results[:beam]

    def _robust_prune(
        self,
        node: int,
        candidates: list[tuple[float, int]],
        ids: list[int],
        vectors: np.ndarray,
        id_to_idx: dict[int, int],
        R: int,
    ) -> list[int]:
        """Robust pruning for diversity (α-RNG rule)."""
        alpha = self._config.alpha
        neighbors: list[int] = []
        sorted_cands = sorted(candidates)

        for dist, cand in sorted_cands:
            if cand == node:
                continue
            if len(neighbors) >= R:
                break
            # Check α-RNG: keep candidate if no existing neighbor is closer
            keep = True
            cand_idx = id_to_idx.get(cand)
            if cand_idx is None:
                continue
            for nb in neighbors:
                nb_idx = id_to_idx.get(nb)
                if nb_idx is None:
                    continue
                nb_dist = float(np.sum(
                    (vectors[cand_idx] - vectors[nb_idx]) ** 2
                ))
                if alpha * nb_dist < dist:
                    keep = False
                    break
            if keep:
                neighbors.append(cand)

        return neighbors

    # ── Search ──────────────────────────────────────────────────────

    def search(self, query: np.ndarray, top_k: int = 10, diversity: float = 0.0) -> list[tuple[int, float]]:
        """Search using PQ beam search + SSD re-ranking.

        1. Build PQ lookup table for query (O(m·k) once)
        2. Beam search on Vamana graph using LUT distances (in RAM)
        3. Load top candidates from SSD → exact re-rank
        4. (Optional) diversity-aware selection via MMR

        Parameters
        ----------
        query : np.ndarray
            Query vector.
        top_k : int
            Number of results to return.
        diversity : float
            MMR diversity factor in [0, 1]. 0 = pure distance, 1 = max diversity.
        """
        if self._count == 0:
            return []

        L = max(self._config.search_beam, top_k * 2)

        # Build distance lookup table once for this query
        lut = self._build_pq_lookup_table(query)

        # Phase 1: PQ-based beam search using LUT
        pq_candidates = self._beam_search_pq_lut(query, lut, L)

        # Phase 2: Load full vectors from SSD and re-rank
        reranked: list[tuple[float, int, np.ndarray]] = []
        for _, vid in pq_candidates[:L]:
            full_vec = self._load_vector_from_disk(vid)
            if full_vec is not None:
                exact_dist = float(np.sum((query - full_vec) ** 2))
                reranked.append((exact_dist, vid, full_vec))

        reranked.sort()

        # Phase 3: Diversity selection (MMR) if requested
        if diversity > 0 and len(reranked) > top_k:
            return self._mmr_select(reranked, top_k, diversity)

        return [(vid, dist) for dist, vid, _ in reranked[:top_k]]

    def _mmr_select(
        self,
        candidates: list[tuple[float, int, np.ndarray]],
        top_k: int,
        lam: float,
    ) -> list[tuple[int, float]]:
        """Maximal Marginal Relevance — balance relevance + diversity.

        Score_i = (1 - λ) · relevance_i  −  λ · max_j∈selected sim(i, j)
        """
        if not candidates:
            return []

        # Normalize distances to [0,1] for scoring
        max_dist = max(d for d, _, _ in candidates) or 1.0
        selected: list[tuple[int, float, np.ndarray]] = []
        remaining = list(range(len(candidates)))

        for _ in range(min(top_k, len(candidates))):
            best_idx = -1
            best_score = float("-inf")

            for i in remaining:
                dist, vid, vec = candidates[i]
                relevance = 1.0 - (dist / max_dist)

                # Max similarity to already selected
                max_sim = 0.0
                for _, _, sel_vec in selected:
                    sim = float(np.dot(vec, sel_vec)) / (
                        float(np.linalg.norm(vec)) * float(np.linalg.norm(sel_vec)) + 1e-12
                    )
                    if sim > max_sim:
                        max_sim = sim

                score = (1 - lam) * relevance - lam * max_sim
                if score > best_score:
                    best_score = score
                    best_idx = i

            if best_idx >= 0:
                d, vid, vec = candidates[best_idx]
                selected.append((vid, d, vec))
                remaining.remove(best_idx)

        return [(vid, dist) for vid, dist, _ in selected]

    def _beam_search_pq_lut(
        self, query: np.ndarray, lut: np.ndarray, beam: int,
    ) -> list[tuple[float, int]]:
        """Beam search on graph using pre-computed PQ lookup table."""
        visited: set[int] = set()
        start = self._medoid
        if start < 0 or start not in self._pq_codes:
            return []

        d = self._pq_distance_lut(lut, self._pq_codes[start])
        heap: list[tuple[float, int]] = [(d, start)]
        visited.add(start)
        results: list[tuple[float, int]] = [(d, start)]

        while heap:
            dist, current = heapq.heappop(heap)
            for nb in self._graph.get(current, []):
                if nb not in visited:
                    visited.add(nb)
                    if nb in self._pq_codes:
                        nd = self._pq_distance_lut(lut, self._pq_codes[nb])
                        heapq.heappush(heap, (nd, nb))
                        results.append((nd, nb))

            if len(visited) >= beam:
                break

        results.sort()
        return results[:beam]

    def _load_vector_from_disk(self, vid: int) -> Optional[np.ndarray]:
        """Load a single vector from SSD storage."""
        offset = self._vec_offsets.get(vid)
        if offset is None:
            return None

        vec_bytes = self._config.dim * 4
        try:
            with open(self._vec_file_path, "rb") as f:
                f.seek(offset)
                raw = f.read(vec_bytes)
            return np.frombuffer(raw, dtype=np.float32).copy()
        except (IOError, ValueError):
            return None

    # ── Single vector insert ────────────────────────────────────────

    def insert(self, vid: int, vector: np.ndarray) -> None:
        """Insert a single vector into the index."""
        vec = vector.astype(np.float32)

        # Write to SSD
        with open(self._vec_file_path, "ab") as f:
            offset = f.tell()
            f.write(vec.tobytes())
        self._vec_offsets[vid] = offset

        # PQ encode
        if self._pq_centroids is not None:
            self._pq_codes[vid] = self._encode_pq(vec)
        else:
            self._pq_codes[vid] = np.zeros(
                self._config.pq_subvectors, dtype=np.uint8
            )

        # Add to graph
        self._graph[vid] = []
        if self._medoid < 0:
            self._medoid = vid
        else:
            # Find nearest neighbors via beam search
            lut = self._build_pq_lookup_table(vec)
            candidates = self._beam_search_pq_lut(vec, lut, self._config.build_beam)
            neighbors = [c[1] for c in candidates[:self._config.max_degree] if c[1] != vid]
            self._graph[vid] = neighbors
            for nb in neighbors:
                if vid not in self._graph.get(nb, []):
                    if len(self._graph.get(nb, [])) < self._config.max_degree:
                        self._graph.setdefault(nb, []).append(vid)

        self._count += 1

    # ── Diagnostics ─────────────────────────────────────────────────

    @property
    def count(self) -> int:
        return self._count

    @property
    def medoid(self) -> int:
        return self._medoid

    def stats(self) -> dict:
        avg_degree = (
            sum(len(nb) for nb in self._graph.values()) / max(len(self._graph), 1)
        )
        ram_bytes = (
            len(self._pq_codes) * self._config.pq_subvectors  # PQ codes
            + sum(len(nb) * 8 for nb in self._graph.values())  # Graph edges
        )
        disk_bytes = self._count * self._config.dim * 4

        return {
            "count": self._count,
            "graph_nodes": len(self._graph),
            "avg_degree": round(avg_degree, 1),
            "ram_bytes": ram_bytes,
            "disk_bytes": disk_bytes,
            "ram_per_vector": ram_bytes // max(self._count, 1),
            "medoid": self._medoid,
        }
