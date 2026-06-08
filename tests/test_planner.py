"""Tests for gateway.query_router.planner — query routing."""

from __future__ import annotations

import pytest

from gateway.query_router.planner import EngineType, QueryPlanner


@pytest.fixture
def planner() -> QueryPlanner:
    return QueryPlanner()


class TestQueryPlanner:
    def test_crud_routes_to_core_db(self, planner: QueryPlanner) -> None:
        plan = planner.plan({"action": "find", "entity": "articles"})
        assert EngineType.CORE_DB in plan.engines

    def test_insert_routes_to_core_db(self, planner: QueryPlanner) -> None:
        plan = planner.plan({"action": "insert", "entity": "articles", "data": {"title": "x"}})
        assert EngineType.CORE_DB in plan.engines

    def test_lexical_search_routes_to_search(self, planner: QueryPlanner) -> None:
        plan = planner.plan({
            "action": "search",
            "collection": "articles",
            "text": "database",
            "strategy": {"lexical": True},
        })
        assert EngineType.SEARCH in plan.engines

    def test_vector_search_routes_to_vector(self, planner: QueryPlanner) -> None:
        plan = planner.plan({
            "action": "search",
            "collection": "articles",
            "text": "database",
            "strategy": {"vector": True},
        })
        assert EngineType.VECTOR in plan.engines

    def test_hybrid_search_routes_both(self, planner: QueryPlanner) -> None:
        plan = planner.plan({
            "action": "search",
            "collection": "articles",
            "text": "database",
            "strategy": {"lexical": True, "vector": True},
        })
        assert EngineType.SEARCH in plan.engines
        assert EngineType.VECTOR in plan.engines

    def test_aggregate_routes_to_analytics(self, planner: QueryPlanner) -> None:
        plan = planner.plan({
            "action": "aggregate",
            "dataset": "events",
            "group_by": ["type"],
            "metrics": [{"count": "*"}],
        })
        assert EngineType.ANALYTICS in plan.engines

    def test_cache_key_generated(self, planner: QueryPlanner) -> None:
        plan = planner.plan({"action": "find", "entity": "articles"})
        assert plan.cache_key is not None and len(plan.cache_key) > 0
