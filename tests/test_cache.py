"""Tests for cache_layer — object cache and query cache."""

from __future__ import annotations

import time

import pytest

from cache_layer.object_cache.lru_cache import CacheEntry, ObjectCache
from cache_layer.query_cache.query_cache import QueryCache


class TestObjectCache:
    def test_put_and_get(self) -> None:
        cache = ObjectCache(max_size=10)
        cache.put("k1", {"a": 1}, version=1, tags=["articles"])
        val = cache.get("k1")
        assert val == {"a": 1}

    def test_eviction(self) -> None:
        cache = ObjectCache(max_size=3)
        cache.put("k1", "v1", version=1)
        cache.put("k2", "v2", version=1)
        cache.put("k3", "v3", version=1)
        cache.put("k4", "v4", version=1)  # should evict k1
        assert cache.get("k1") is None
        assert cache.get("k4") == "v4"

    def test_ttl_expiration(self) -> None:
        cache = ObjectCache(max_size=10, default_ttl=0.1)
        cache.put("k1", "v1", version=1)
        time.sleep(0.15)
        assert cache.get("k1") is None

    def test_invalidate_by_tag(self) -> None:
        cache = ObjectCache(max_size=10)
        cache.put("k1", "v1", version=1, tags=["articles"])
        cache.put("k2", "v2", version=1, tags=["articles"])
        cache.put("k3", "v3", version=1, tags=["users"])
        cache.invalidate_by_tag("articles")
        assert cache.get("k1") is None
        assert cache.get("k2") is None
        assert cache.get("k3") == "v3"

    def test_version_stale_check(self) -> None:
        cache = ObjectCache(max_size=10)
        cache.put("k1", "v_old", version=1, tags=["x"])
        assert cache.invalidate_if_stale("k1", current_version=2) is True
        assert cache.get("k1") is None

    def test_stats(self) -> None:
        cache = ObjectCache(max_size=10)
        cache.put("k1", "v1", version=1)
        cache.get("k1")
        cache.get("missing")
        s = cache.stats()
        assert s["hits"] == 1
        assert s["misses"] == 1


class TestQueryCache:
    def test_put_and_get(self) -> None:
        qc = QueryCache(max_size=10)
        qc.put("articles", {"action": "find"}, [{"id": "a1"}])
        result = qc.get("articles", {"action": "find"})
        assert result == [{"id": "a1"}]

    def test_invalidate_collection(self) -> None:
        qc = QueryCache(max_size=10)
        qc.put("articles", {"action": "find"}, [{"id": "a1"}])
        qc.invalidate_collection("articles")
        assert qc.get("articles", {"action": "find"}) is None
