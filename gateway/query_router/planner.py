"""Query planner and router for Schema Action requests."""

from __future__ import annotations

from dataclasses import dataclass, field
from enum import Enum
from typing import Any


class EngineType(Enum):
    CORE_DB = "core_db"
    SEARCH = "search"
    VECTOR = "vector"
    ANALYTICS = "analytics"
    CACHE = "cache"


class SearchMode(Enum):
    LEXICAL = "lexical"
    VECTOR = "vector"
    HYBRID = "hybrid"
    RERANKED = "reranked"


@dataclass
class QueryPlan:
    engines: list[EngineType]
    primary_engine: EngineType
    use_cache: bool = False
    cache_key: str | None = None
    search_mode: SearchMode | None = None
    index_hints: list[str] = field(default_factory=list)
    pushdown_filters: dict[str, Any] = field(default_factory=dict)
    merge_strategy: str = "passthrough"
    late_materialize: bool = False

    def to_dict(self) -> dict[str, Any]:
        return {
            "engines": [e.value for e in self.engines],
            "primary_engine": self.primary_engine.value,
            "use_cache": self.use_cache,
            "cache_key": self.cache_key,
            "search_mode": self.search_mode.value if self.search_mode else None,
            "index_hints": self.index_hints,
            "pushdown_filters": self.pushdown_filters,
            "merge_strategy": self.merge_strategy,
            "late_materialize": self.late_materialize,
        }


class QueryPlanner:
    @staticmethod
    def _get_attr(request: Any, key: str, default: Any = "") -> Any:
        if isinstance(request, dict):
            return request.get(key, default)
        return getattr(request, key, default)

    def plan(self, request: Any) -> QueryPlan:
        action = self._get_attr(request, "action", "")
        if action in ("find", "get", "insert", "update", "upsert", "delete"):
            return self._plan_crud(request)
        if action == "search":
            return self._plan_search(request)
        if action == "aggregate":
            return self._plan_analytics(request)
        return QueryPlan(engines=[EngineType.CORE_DB], primary_engine=EngineType.CORE_DB)

    def _plan_crud(self, request: Any) -> QueryPlan:
        plan = QueryPlan(engines=[EngineType.CORE_DB], primary_engine=EngineType.CORE_DB)
        if self._get_attr(request, "action", "") in ("find", "get"):
            plan.use_cache = True
            plan.cache_key = self._compute_cache_key(request)
        return plan

    def _plan_search(self, request: Any) -> QueryPlan:
        strategy = self._get_attr(request, "strategy", None) or {}
        where = self._get_attr(request, "where", None) or {}
        use_lexical = strategy.get("lexical", True) or "query" in where
        use_vector = strategy.get("vector", False)
        use_rerank = strategy.get("rerank", False)

        engines: list[EngineType] = []
        if use_lexical:
            engines.append(EngineType.SEARCH)
        if use_vector:
            engines.append(EngineType.VECTOR)

        search_mode = SearchMode.LEXICAL
        if use_vector and use_lexical:
            search_mode = SearchMode.HYBRID
        elif use_vector:
            search_mode = SearchMode.VECTOR
        if use_rerank:
            search_mode = SearchMode.RERANKED

        return QueryPlan(
            engines=engines or [EngineType.SEARCH],
            primary_engine=(engines or [EngineType.SEARCH])[0],
            search_mode=search_mode,
            merge_strategy="rerank" if use_rerank else ("union" if len(engines) > 1 else "passthrough"),
            use_cache=True,
            late_materialize=True,
        )

    def _plan_analytics(self, request: Any) -> QueryPlan:
        return QueryPlan(
            engines=[EngineType.ANALYTICS],
            primary_engine=EngineType.ANALYTICS,
            pushdown_filters=self._get_attr(request, "where", None) or {},
        )

    def _compute_cache_key(self, request: Any) -> str:
        import hashlib
        import json

        payload = {
            "action": self._get_attr(request, "action", ""),
            "entity": self._get_attr(request, "entity", ""),
            "where": self._get_attr(request, "where", None),
            "limit": self._get_attr(request, "limit", 20),
            "offset": self._get_attr(request, "offset", 0),
        }
        raw = json.dumps(payload, sort_keys=True, default=str)
        return hashlib.sha256(raw.encode()).hexdigest()[:16]

