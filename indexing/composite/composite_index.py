"""QM Indexing — Composite index.

Multi-column index with:
  - Equality columns first, range columns after, sort columns last
  - Partial index support (filtered subset)
  - Covering index support (included columns)
  - Index recommendation based on query patterns
"""

from __future__ import annotations

from dataclasses import dataclass, field
from typing import Any


@dataclass
class CompositeIndexDef:
    """Definition of a composite (multi-column) index."""

    name: str
    columns: list[str]  # Ordered: equality → range → sort
    unique: bool = False
    partial_filter: str | None = None  # e.g. "is_deleted = false"
    include_columns: list[str] | None = None  # Covering index columns
    sort_orders: list[str] | None = None  # "asc" or "desc" per column

    @property
    def is_partial(self) -> bool:
        return self.partial_filter is not None

    @property
    def is_covering(self) -> bool:
        return self.include_columns is not None and len(self.include_columns) > 0


class IndexRecommender:
    """Recommends indexes based on observed query patterns.

    Rules:
      - Equality columns first
      - Range columns after equality
      - Sort columns last
      - Avoid redundant overlapping indexes
      - Prefer partial indexes when filter is selective
    """

    def __init__(self) -> None:
        self._query_patterns: list[dict[str, Any]] = []
        self._existing_indexes: list[CompositeIndexDef] = []

    def record_query(self, pattern: dict[str, Any]) -> None:
        """Record a query pattern for analysis.

        Pattern format:
        {
            "table": "articles",
            "equality": ["tenant_id", "status"],
            "range": ["created_at"],
            "sort": [("created_at", "desc")],
            "count": 1,
        }
        """
        # Merge with existing pattern or add new
        for existing in self._query_patterns:
            if (
                existing.get("table") == pattern.get("table")
                and existing.get("equality") == pattern.get("equality")
                and existing.get("range") == pattern.get("range")
            ):
                existing["count"] = existing.get("count", 0) + pattern.get("count", 1)
                return
        self._query_patterns.append(pattern)

    def register_existing(self, index_def: CompositeIndexDef) -> None:
        self._existing_indexes.append(index_def)

    def recommend(self, min_count: int = 5) -> list[CompositeIndexDef]:
        """Generate index recommendations."""
        recommendations: list[CompositeIndexDef] = []

        # Sort patterns by frequency
        sorted_patterns = sorted(
            self._query_patterns,
            key=lambda p: p.get("count", 0),
            reverse=True,
        )

        for pattern in sorted_patterns:
            if pattern.get("count", 0) < min_count:
                continue

            columns: list[str] = []
            sort_orders: list[str] = []

            # Equality first
            for col in pattern.get("equality", []):
                columns.append(col)
                sort_orders.append("asc")

            # Range next
            for col in pattern.get("range", []):
                columns.append(col)
                sort_orders.append("asc")

            # Sort last
            for col, order in pattern.get("sort", []):
                if col not in columns:
                    columns.append(col)
                    sort_orders.append(order)

            if not columns:
                continue

            # Check if already covered
            if self._is_covered(columns):
                continue

            table = pattern.get("table", "unknown")
            name = f"idx_{table}_{'_'.join(columns)}"
            recommendations.append(CompositeIndexDef(
                name=name,
                columns=columns,
                sort_orders=sort_orders,
            ))

        return recommendations

    def _is_covered(self, columns: list[str]) -> bool:
        """Check if these columns are already covered by an existing index."""
        for idx in self._existing_indexes:
            if columns == idx.columns[:len(columns)]:
                return True
        return False
