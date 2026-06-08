"""QM Vector Platform — Embedding Store.

Stores raw and quantized vectors with metadata:
  - Raw fp32 vectors
  - Quantized fp16/int8/PQ representations
  - Metadata per vector (doc_id, namespace, tags)
  - Segment-based storage for background compaction
"""

from __future__ import annotations

import numpy as np
from dataclasses import dataclass, field
from typing import Any


@dataclass
class VectorEntry:
    """A single stored vector with metadata."""

    vector_id: str
    vector: np.ndarray  # raw embedding
    metadata: dict[str, Any] = field(default_factory=dict)
    namespace: str = "default"
    quantized: np.ndarray | None = None  # quantized version


@dataclass
class VectorSegment:
    """A segment of vectors (for compaction / delta indexing)."""

    segment_id: str
    entries: list[VectorEntry] = field(default_factory=list)
    is_sealed: bool = False
    created_at: float = 0.0

    @property
    def size(self) -> int:
        return len(self.entries)

    def add(self, entry: VectorEntry) -> None:
        if self.is_sealed:
            raise RuntimeError(f"Segment {self.segment_id} is sealed")
        self.entries.append(entry)

    def seal(self) -> None:
        self.is_sealed = True


class EmbeddingStore:
    """Manages vector storage across segments and namespaces."""

    def __init__(self, dimension: int) -> None:
        self.dimension = dimension
        self._segments: dict[str, VectorSegment] = {}
        self._active_segment_id: str | None = None
        self._vector_map: dict[str, VectorEntry] = {}  # vector_id -> entry
        self._segment_counter = 0
        self._max_segment_size = 10_000

    def _get_active_segment(self) -> VectorSegment:
        """Get or create the active (writable) segment."""
        if self._active_segment_id:
            seg = self._segments[self._active_segment_id]
            if seg.size < self._max_segment_size:
                return seg
            seg.seal()

        import time
        self._segment_counter += 1
        seg_id = f"seg_{self._segment_counter}"
        seg = VectorSegment(segment_id=seg_id, created_at=time.time())
        self._segments[seg_id] = seg
        self._active_segment_id = seg_id
        return seg

    def insert(
        self,
        vector_id: str,
        vector: np.ndarray,
        metadata: dict[str, Any] | None = None,
        namespace: str = "default",
    ) -> None:
        """Insert a vector into the store."""
        if vector.shape[0] != self.dimension:
            raise ValueError(
                f"Vector dimension {vector.shape[0]} != expected {self.dimension}"
            )

        entry = VectorEntry(
            vector_id=vector_id,
            vector=vector.astype(np.float32),
            metadata=metadata or {},
            namespace=namespace,
        )

        seg = self._get_active_segment()
        seg.add(entry)
        self._vector_map[vector_id] = entry

    def get(self, vector_id: str) -> VectorEntry | None:
        return self._vector_map.get(vector_id)

    def delete(self, vector_id: str) -> bool:
        entry = self._vector_map.pop(vector_id, None)
        return entry is not None

    def get_all_vectors(self, namespace: str | None = None) -> list[VectorEntry]:
        """Get all vectors, optionally filtered by namespace."""
        if namespace is None:
            return list(self._vector_map.values())
        return [v for v in self._vector_map.values() if v.namespace == namespace]

    def get_vectors_matrix(self, namespace: str | None = None) -> tuple[list[str], np.ndarray]:
        """Get all vectors as a matrix for batch operations."""
        entries = self.get_all_vectors(namespace)
        if not entries:
            return [], np.array([]).reshape(0, self.dimension)

        ids = [e.vector_id for e in entries]
        matrix = np.stack([e.vector for e in entries])
        return ids, matrix

    @property
    def total_vectors(self) -> int:
        return len(self._vector_map)

    @property
    def segment_count(self) -> int:
        return len(self._segments)
