"""QM Indexing — Hash index.

Simple hash index for pure equality lookups.
Generally B-tree is preferred unless equality-only workload is dominant.
"""

from __future__ import annotations

from collections import defaultdict
from typing import Any


class HashIndex:
    """Hash-based index for O(1) equality lookups."""

    def __init__(self, name: str = "") -> None:
        self.name = name
        self._buckets: dict[int, list[tuple[Any, Any]]] = defaultdict(list)
        self._size = 0

    def insert(self, key: Any, value: Any) -> None:
        h = hash(key)
        self._buckets[h].append((key, value))
        self._size += 1

    def get(self, key: Any) -> list[Any]:
        """Get all values for a key."""
        h = hash(key)
        return [v for k, v in self._buckets[h] if k == key]

    def get_one(self, key: Any) -> Any | None:
        """Get first value for a key."""
        results = self.get(key)
        return results[0] if results else None

    def delete(self, key: Any) -> int:
        """Delete all entries for a key. Returns count deleted."""
        h = hash(key)
        original = len(self._buckets[h])
        self._buckets[h] = [(k, v) for k, v in self._buckets[h] if k != key]
        deleted = original - len(self._buckets[h])
        self._size -= deleted
        return deleted

    @property
    def size(self) -> int:
        return self._size
