"""QM Pipelines — Vector Indexer.

Consumes CDC events and updates vector indexes:
  - Generate embeddings (via external model or callback)
  - Insert into ANN index
  - Handle updates and deletes
"""

from __future__ import annotations

from dataclasses import dataclass
from typing import Any, Callable

import numpy as np

from vector_platform.embedding_store.store import EmbeddingStore
from vector_platform.ann_index.hnsw import BruteForceIndex, ANNResult
from core_db.wal_cdc.wal import CDCEvent


EmbeddingFunction = Callable[[str], np.ndarray]


class VectorIndexer:
    """Indexes vectors from CDC events into the vector engine."""

    def __init__(
        self,
        store: EmbeddingStore,
        index: BruteForceIndex,
        embed_fn: EmbeddingFunction | None = None,
    ) -> None:
        self._store = store
        self._index = index
        self._embed_fn = embed_fn
        self._indexed_count = 0

    def set_embedding_function(self, fn: EmbeddingFunction) -> None:
        self._embed_fn = fn

    def handle_cdc_event(self, event: CDCEvent) -> bool:
        """Process a CDC event for vector indexing."""
        try:
            if event.operation == "delete":
                self._store.delete(event.pk)
                self._index.remove(event.pk)
                self._indexed_count += 1
                return True

            if event.operation in ("insert", "update"):
                if event.new_data and self._embed_fn:
                    text = self._extract_text(event.new_data)
                    if text:
                        vector = self._embed_fn(text)
                        metadata = {
                            k: v for k, v in event.new_data.items()
                            if not isinstance(v, (bytes, memoryview))
                        }
                        self._store.insert(event.pk, vector, metadata)
                        self._index.add(event.pk, vector, metadata)
                        self._indexed_count += 1
                return True

            return True
        except Exception:
            return False

    def index_vector(
        self,
        vector_id: str,
        vector: np.ndarray,
        metadata: dict[str, Any] | None = None,
    ) -> None:
        """Directly index a vector."""
        self._store.insert(vector_id, vector, metadata)
        self._index.add(vector_id, vector, metadata)
        self._indexed_count += 1

    def _extract_text(self, data: dict[str, Any]) -> str | None:
        """Extract text content from event data for embedding."""
        parts: list[str] = []
        for key, value in data.items():
            if isinstance(value, str) and len(value) > 10:
                parts.append(value)
        return " ".join(parts) if parts else None

    @property
    def stats(self) -> dict[str, int]:
        return {"indexed": self._indexed_count, "store_size": self._store.total_vectors}
