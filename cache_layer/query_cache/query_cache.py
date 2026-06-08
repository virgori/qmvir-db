"""QM Cache Layer — Query result cache.

Caches:
  - Filter/query results (find, list)
  - Search result hit lists
  - Vector candidate results
  - Materialized view outputs
"""

from __future__ import annotations

import hashlib
import json
import time
import threading
from collections import OrderedDict
from dataclasses import dataclass, field
from typing import Any


@dataclass
class QueryCacheEntry:
    """Cached query result."""

    cache_key: str
    result: Any
    collection: str
    query_hash: str
    created_at: float = field(default_factory=time.time)
    expires_at: float = 0.0
    collection_version: int = 0

    @property
    def is_expired(self) -> bool:
        return time.time() > self.expires_at


class QueryCache:
    """Caches query results with collection-version-based invalidation."""

    def __init__(self, max_entries: int = 5_000, default_ttl_s: float = 60.0, max_size: int | None = None) -> None:
        self._max_entries = max_size if max_size is not None else max_entries
        self._default_ttl_s = default_ttl_s
        self._store: OrderedDict[str, QueryCacheEntry] = OrderedDict()
        self._collection_versions: dict[str, int] = {}
        self._lock = threading.Lock()

    @staticmethod
    def compute_key(collection: str, query: dict[str, Any]) -> str:
        """Compute deterministic cache key for a query."""
        raw = json.dumps({"c": collection, "q": query}, sort_keys=True, default=str)
        return hashlib.sha256(raw.encode()).hexdigest()[:20]

    def get(self, collection: str, query: dict[str, Any]) -> Any | None:
        """Look up a cached query result."""
        key = self.compute_key(collection, query)
        with self._lock:
            entry = self._store.get(key)
            if entry is None:
                return None
            if entry.is_expired:
                del self._store[key]
                return None
            # Check collection version
            current_ver = self._collection_versions.get(collection, 0)
            if entry.collection_version < current_ver:
                del self._store[key]
                return None

            self._store.move_to_end(key)
            return entry.result

    def put(
        self,
        collection: str,
        query: dict[str, Any],
        result: Any,
        ttl_s: float | None = None,
    ) -> None:
        """Cache a query result."""
        key = self.compute_key(collection, query)
        ttl = ttl_s if ttl_s is not None else self._default_ttl_s

        entry = QueryCacheEntry(
            cache_key=key,
            result=result,
            collection=collection,
            query_hash=key,
            expires_at=time.time() + ttl,
            collection_version=self._collection_versions.get(collection, 0),
        )

        with self._lock:
            self._store[key] = entry
            while len(self._store) > self._max_entries:
                self._store.popitem(last=False)

    def invalidate_collection(self, collection: str) -> None:
        """Bump collection version to invalidate all cached queries for it."""
        with self._lock:
            self._collection_versions[collection] = (
                self._collection_versions.get(collection, 0) + 1
            )

    def clear(self) -> None:
        with self._lock:
            self._store.clear()
