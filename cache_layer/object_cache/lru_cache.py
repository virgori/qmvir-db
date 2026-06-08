"""QM Cache Layer — Object cache (entity by ID).

LRU cache with:
  - TTL expiration
  - Version-based invalidation
  - Size-limited eviction
  - Tag-based bulk invalidation
"""

from __future__ import annotations

import time
import threading
from collections import OrderedDict
from dataclasses import dataclass, field
from typing import Any


@dataclass
class CacheEntry:
    """A single cache entry."""

    key: str
    value: Any
    version: int = 1
    tags: set[str] = field(default_factory=set)
    created_at: float = field(default_factory=time.time)
    expires_at: float | None = None
    access_count: int = 0
    last_accessed: float = field(default_factory=time.time)

    @property
    def is_expired(self) -> bool:
        if self.expires_at is None:
            return False
        return time.time() > self.expires_at


class ObjectCache:
    """LRU object cache for entity-by-ID lookups."""

    def __init__(self, max_size: int = 10_000, default_ttl_s: float = 300.0, default_ttl: float | None = None) -> None:
        self._max_size = max_size
        self._default_ttl_s = default_ttl if default_ttl is not None else default_ttl_s
        self._store: OrderedDict[str, CacheEntry] = OrderedDict()
        self._lock = threading.Lock()
        self._stats = {"hits": 0, "misses": 0, "evictions": 0}

    def get(self, key: str) -> Any | None:
        """Get a value by key. Returns None on miss or expiry."""
        with self._lock:
            entry = self._store.get(key)
            if entry is None:
                self._stats["misses"] += 1
                return None
            if entry.is_expired:
                del self._store[key]
                self._stats["misses"] += 1
                return None

            entry.access_count += 1
            entry.last_accessed = time.time()
            self._store.move_to_end(key)
            self._stats["hits"] += 1
            return entry.value

    def put(
        self,
        key: str,
        value: Any,
        ttl_s: float | None = None,
        version: int = 1,
        tags: set[str] | list | None = None,
    ) -> None:
        """Put a value into cache."""
        ttl = ttl_s if ttl_s is not None else self._default_ttl_s
        tag_set = set(tags) if tags is not None else set()
        entry = CacheEntry(
            key=key,
            value=value,
            version=version,
            tags=tag_set,
            expires_at=time.time() + ttl if ttl > 0 else None,
        )

        with self._lock:
            if key in self._store:
                del self._store[key]
            self._store[key] = entry

            while len(self._store) > self._max_size:
                self._store.popitem(last=False)
                self._stats["evictions"] += 1

    def invalidate(self, key: str) -> bool:
        """Invalidate a single key."""
        with self._lock:
            if key in self._store:
                del self._store[key]
                return True
            return False

    def invalidate_by_tag(self, tag: str) -> int:
        """Invalidate all entries with a given tag."""
        count = 0
        with self._lock:
            keys_to_remove = [
                k for k, v in self._store.items() if tag in v.tags
            ]
            for k in keys_to_remove:
                del self._store[k]
                count += 1
        return count

    def invalidate_if_stale(self, key: str, current_version: int) -> bool:
        """Invalidate if cached version is older than current."""
        with self._lock:
            entry = self._store.get(key)
            if entry and entry.version < current_version:
                del self._store[key]
                return True
            return False

    def clear(self) -> None:
        with self._lock:
            self._store.clear()

    @property
    def size(self) -> int:
        return len(self._store)

    def stats(self) -> dict[str, int]:
        return dict(self._stats)
