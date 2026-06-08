"""QM Cache Layer — Event-based cache invalidation.

Listens to CDC events and invalidates cache entries accordingly.
Supports:
  - Key-based invalidation
  - Tag-based invalidation
  - Collection-version bumping
  - TTL-based expiry as fallback
"""

from __future__ import annotations

from dataclasses import dataclass
from typing import Any

from cache_layer.object_cache.lru_cache import ObjectCache
from cache_layer.query_cache.query_cache import QueryCache


@dataclass
class InvalidationEvent:
    """An event that triggers cache invalidation."""

    operation: str  # insert, update, delete
    table: str
    pk: str
    changed_fields: list[str] | None = None


class CacheInvalidator:
    """Processes CDC events and invalidates relevant caches."""

    def __init__(
        self,
        object_cache: ObjectCache,
        query_cache: QueryCache,
    ) -> None:
        self._object_cache = object_cache
        self._query_cache = query_cache
        self._invalidation_count = 0

    def handle_cdc_event(self, event: InvalidationEvent) -> None:
        """Handle a CDC event and invalidate affected caches."""
        self._invalidation_count += 1

        # Always invalidate the object cache for this entity
        cache_key = f"{event.table}:{event.pk}"
        self._object_cache.invalidate(cache_key)

        # Invalidate by table tag
        self._object_cache.invalidate_by_tag(event.table)

        # Bump collection version in query cache
        self._query_cache.invalidate_collection(event.table)

    def handle_batch(self, events: list[InvalidationEvent]) -> int:
        """Handle a batch of CDC events. Returns count invalidated."""
        tables_seen: set[str] = set()
        count = 0

        for event in events:
            cache_key = f"{event.table}:{event.pk}"
            self._object_cache.invalidate(cache_key)
            count += 1

            if event.table not in tables_seen:
                self._query_cache.invalidate_collection(event.table)
                tables_seen.add(event.table)

        self._invalidation_count += count
        return count

    @property
    def total_invalidations(self) -> int:
        return self._invalidation_count
