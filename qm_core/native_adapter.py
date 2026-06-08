"""QM Native Adapter — Auto-switch between Rust and Python implementations.

This module provides a unified interface that:
1. Uses Rust native extensions when available (fast path)
2. Falls back to pure Python implementations (slow path)

Usage:
    from qm_core.native_adapter import get_hnsw_index, get_distance_fn

    # Automatically uses Rust if available
    index = get_hnsw_index(dim=128)
    dist_fn = get_distance_fn("cosine")
"""

from __future__ import annotations

import logging
from typing import TYPE_CHECKING, Any, Callable

import numpy as np

logger = logging.getLogger(__name__)

# Try to import native module
_NATIVE_AVAILABLE = False
_native_module = None

try:
    from qm_native import (
        NATIVE_AVAILABLE,
        HNSWIndex as NativeHNSWIndex,
        SearchResult as NativeSearchResult,
        XORDeltaCodec as NativeXORDeltaCodec,
        XORDeltaBatchCodec as NativeXORDeltaBatchCodec,
        cosine_distance as native_cosine_distance,
        l2_distance as native_l2_distance,
        batch_cosine_distances as native_batch_cosine_distances,
        batch_l2_distances as native_batch_l2_distances,
    )
    _NATIVE_AVAILABLE = NATIVE_AVAILABLE
    if _NATIVE_AVAILABLE:
        logger.info("QM Native extensions loaded successfully")
except ImportError as e:
    logger.warning(f"QM Native extensions not available: {e}")
    _NATIVE_AVAILABLE = False


def is_native_available() -> bool:
    """Check if native extensions are available."""
    return _NATIVE_AVAILABLE


# =============================================================================
# Distance Functions
# =============================================================================

def _python_cosine_distance(a: np.ndarray, b: np.ndarray) -> float:
    """Pure Python cosine distance."""
    dot = float(np.dot(a, b))
    na = float(np.linalg.norm(a))
    nb = float(np.linalg.norm(b))
    if na < 1e-10 or nb < 1e-10:
        return 1.0
    return 1.0 - dot / (na * nb)


def _python_l2_distance(a: np.ndarray, b: np.ndarray) -> float:
    """Pure Python L2 distance."""
    return float(np.linalg.norm(a - b))


def _python_batch_cosine_distances(query: np.ndarray, vectors: np.ndarray) -> np.ndarray:
    """Pure Python batch cosine distances."""
    dots = vectors @ query
    norms_v = np.linalg.norm(vectors, axis=1)
    norm_q = np.linalg.norm(query)
    return 1.0 - dots / (norms_v * norm_q + 1e-10)


def _python_batch_l2_distances(query: np.ndarray, vectors: np.ndarray) -> np.ndarray:
    """Pure Python batch L2 distances."""
    return np.linalg.norm(vectors - query, axis=1)


def get_distance_fn(metric: str = "cosine") -> Callable[[np.ndarray, np.ndarray], float]:
    """Get distance function (native if available)."""
    if _NATIVE_AVAILABLE:
        if metric == "cosine":
            return native_cosine_distance
        elif metric in ("l2", "euclidean"):
            return native_l2_distance
    
    # Fallback to Python
    if metric == "cosine":
        return _python_cosine_distance
    elif metric in ("l2", "euclidean"):
        return _python_l2_distance
    else:
        raise ValueError(f"Unknown metric: {metric}")


def get_batch_distance_fn(metric: str = "cosine") -> Callable[[np.ndarray, np.ndarray], np.ndarray]:
    """Get batch distance function (native if available)."""
    if _NATIVE_AVAILABLE:
        if metric == "cosine":
            return native_batch_cosine_distances
        elif metric in ("l2", "euclidean"):
            return native_batch_l2_distances
    
    # Fallback to Python
    if metric == "cosine":
        return _python_batch_cosine_distances
    elif metric in ("l2", "euclidean"):
        return _python_batch_l2_distances
    else:
        raise ValueError(f"Unknown metric: {metric}")


# =============================================================================
# HNSW Index Adapter
# =============================================================================

class HNSWIndexAdapter:
    """HNSW Index that auto-switches between native and Python implementations."""
    
    def __init__(
        self,
        dim: int,
        metric: str = "cosine",
        M: int = 16,
        ef_construction: int = 200,
    ):
        self.dim = dim
        self.metric = metric
        self.M = M
        self.ef_construction = ef_construction
        self._use_native = _NATIVE_AVAILABLE
        
        metric_code = {"cosine": 0, "euclidean": 1, "l2": 1, "inner_product": 2}.get(metric, 0)
        
        if self._use_native:
            logger.debug("Using native HNSW index")
            self._index = NativeHNSWIndex(dim, M, ef_construction, metric)
        else:
            logger.debug("Using Python HNSW index (slower)")
            from qm_core.index.hnsw import HNSWIndex as PythonHNSWIndex, Metric
            metric_enum = {
                "cosine": Metric.COSINE,
                "euclidean": Metric.EUCLIDEAN,
                "l2": Metric.EUCLIDEAN,
                "inner_product": Metric.INNER_PRODUCT,
            }.get(metric, Metric.COSINE)
            self._index = PythonHNSWIndex(dim, metric_enum, M, ef_construction)
    
    def add(self, id: int, vector: np.ndarray, metadata: dict | None = None) -> None:
        """Add a vector to the index."""
        vec = np.asarray(vector, dtype=np.float32)
        if self._use_native:
            self._index.add(id, vec)
        else:
            self._index.add(id, vec, metadata)
    
    def add_batch(self, ids: np.ndarray, vectors: np.ndarray) -> None:
        """Add multiple vectors."""
        ids = np.asarray(ids, dtype=np.int64)
        vectors = np.asarray(vectors, dtype=np.float32)
        
        if self._use_native:
            self._index.add_batch(ids, vectors)
        else:
            for i, vid in enumerate(ids):
                self._index.add(int(vid), vectors[i])
    
    def search(
        self,
        query: np.ndarray,
        top_k: int = 10,
        ef_search: int = 50,
    ) -> list[dict]:
        """Search for nearest neighbors."""
        query = np.asarray(query, dtype=np.float32)
        
        if self._use_native:
            results = self._index.search(query, top_k, ef_search)
            return [
                {"id": r.id, "distance": r.distance, "score": r.score}
                for r in results
            ]
        else:
            results = self._index.search(query, top_k, ef_search)
            return [
                {"id": r.id, "distance": r.distance, "score": r.score}
                for r in results
            ]
    
    def batch_search(
        self,
        queries: np.ndarray,
        top_k: int = 10,
        ef_search: int = 50,
    ) -> list[list[dict]]:
        """Batch search (parallel if native)."""
        queries = np.asarray(queries, dtype=np.float32)
        
        if self._use_native:
            batch_results = self._index.batch_search(queries, top_k, ef_search)
            return [
                [{"id": r.id, "distance": r.distance, "score": r.score} for r in results]
                for results in batch_results
            ]
        else:
            # Sequential fallback
            return [self.search(q, top_k, ef_search) for q in queries]
    
    @property
    def size(self) -> int:
        """Number of vectors in index."""
        return self._index.size if self._use_native else self._index._size
    
    def stats(self) -> dict:
        """Get index statistics."""
        if self._use_native:
            return self._index.stats()
        else:
            return {
                "size": self._index._size,
                "dim": self.dim,
                "m": self.M,
                "ef_construction": self.ef_construction,
                "max_level": self._index._max_level,
                "metric": self.metric,
            }


def get_hnsw_index(
    dim: int,
    metric: str = "cosine",
    M: int = 16,
    ef_construction: int = 200,
) -> HNSWIndexAdapter:
    """Create an HNSW index (automatically uses native if available)."""
    return HNSWIndexAdapter(dim, metric, M, ef_construction)


# =============================================================================
# Compression Adapters
# =============================================================================

class XORDeltaCodecAdapter:
    """XOR-Delta codec that auto-switches between native and Python."""
    
    def __init__(self, dim: int):
        self.dim = dim
        self._use_native = _NATIVE_AVAILABLE
        
        if self._use_native:
            self._codec = NativeXORDeltaCodec(dim)
        else:
            from qm_core.compression import XORDeltaCodec as PythonXORDeltaCodec
            self._codec = PythonXORDeltaCodec(dim)
    
    def encode(self, vector: np.ndarray) -> bytes:
        """Encode a vector."""
        vec = np.asarray(vector, dtype=np.float32)
        if self._use_native:
            import sys
            # Native returns PyBytes, convert to bytes
            result = self._codec.encode(vec)
            return bytes(result)
        else:
            return self._codec.encode(vec)
    
    def decode(self, data: bytes) -> np.ndarray:
        """Decode compressed data."""
        if self._use_native:
            return np.asarray(self._codec.decode(data))
        else:
            return self._codec.decode(data)


class XORDeltaBatchCodecAdapter:
    """XOR-Delta batch codec adapter."""
    
    def __init__(self, dim: int):
        self.dim = dim
        self._use_native = _NATIVE_AVAILABLE
        
        if self._use_native:
            self._codec = NativeXORDeltaBatchCodec(dim)
        else:
            from qm_core.compression import XORDeltaBatchCodec as PythonXORDeltaBatchCodec
            self._codec = PythonXORDeltaBatchCodec(dim)
    
    def encode_batch(self, vectors: np.ndarray) -> bytes:
        """Encode a batch of vectors."""
        vectors = np.asarray(vectors, dtype=np.float32)
        if self._use_native:
            result = self._codec.encode_batch(vectors)
            return bytes(result)
        else:
            return self._codec.encode_batch(vectors)
    
    def decode_batch(self, data: bytes, num_vectors: int) -> np.ndarray:
        """Decode a batch."""
        if self._use_native:
            return np.asarray(self._codec.decode_batch(data, num_vectors))
        else:
            return self._codec.decode_batch(data, num_vectors)


def get_xor_delta_codec(dim: int) -> XORDeltaCodecAdapter:
    """Get XOR-Delta codec (native if available)."""
    return XORDeltaCodecAdapter(dim)


def get_xor_delta_batch_codec(dim: int) -> XORDeltaBatchCodecAdapter:
    """Get XOR-Delta batch codec (native if available)."""
    return XORDeltaBatchCodecAdapter(dim)


# =============================================================================
# Performance Info
# =============================================================================

def get_backend_info() -> dict:
    """Get information about the active backend."""
    info = {
        "native_available": _NATIVE_AVAILABLE,
        "backend": "rust" if _NATIVE_AVAILABLE else "python",
    }
    
    if _NATIVE_AVAILABLE:
        try:
            from qm_native import check_simd_support
            info["simd"] = check_simd_support()
        except Exception:
            pass
    
    return info
