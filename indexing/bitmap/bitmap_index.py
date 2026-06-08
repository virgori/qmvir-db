"""QM Indexing — Bitmap / Roaring Bitmap index.

Optimized for:
  - Low-cardinality columns (status, type, category)
  - Faceted search / tag filters
  - Analytics filter intersection
  - Cohort selection
"""

from __future__ import annotations

from collections import defaultdict
from typing import Any


class BitmapIndex:
    """Simple bitmap index for low-cardinality columns."""

    def __init__(self, name: str = "") -> None:
        self.name = name
        # value -> set of row_ids
        self._bitmaps: dict[Any, set[int]] = defaultdict(set)
        self._row_count = 0

    def add(self, row_id: int, value: Any) -> None:
        """Add a value for a row."""
        self._bitmaps[value].add(row_id)
        self._row_count = max(self._row_count, row_id + 1)

    def get(self, value: Any) -> set[int]:
        """Get all row IDs matching a value."""
        return self._bitmaps.get(value, set())

    def intersect(self, values: list[Any]) -> set[int]:
        """AND intersection across multiple values."""
        if not values:
            return set()
        result = self.get(values[0])
        for v in values[1:]:
            result = result & self.get(v)
        return result

    def union(self, values: list[Any]) -> set[int]:
        """OR union across multiple values."""
        result: set[int] = set()
        for v in values:
            result |= self.get(v)
        return result

    def not_equal(self, value: Any) -> set[int]:
        """All row IDs NOT matching value."""
        excluded = self.get(value)
        all_ids: set[int] = set()
        for bitmap in self._bitmaps.values():
            all_ids |= bitmap
        return all_ids - excluded

    def cardinality(self, value: Any) -> int:
        """Count of rows with this value."""
        return len(self._bitmaps.get(value, set()))

    def distinct_values(self) -> list[Any]:
        """Get all distinct values (for facets)."""
        return list(self._bitmaps.keys())

    def facet_counts(self) -> dict[Any, int]:
        """Get value → count mapping (for faceted search)."""
        return {v: len(ids) for v, ids in self._bitmaps.items()}

    @property
    def size(self) -> int:
        return sum(len(s) for s in self._bitmaps.values())
