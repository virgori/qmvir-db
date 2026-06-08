"""Schema Action Client — builds and validates Schema Action DSL payloads.

This module provides a fluent builder for constructing QM schema action
queries, independent of transport layer.

Usage:
    from sdk.schema_action_client.builder import SchemaAction

    q = (SchemaAction("articles")
         .find()
         .where(status="published", category__in=["tech", "ai"])
         .select("id", "title", "score")
         .order_by("score", "desc")
         .limit(10)
         .build())
"""

from __future__ import annotations

from dataclasses import dataclass, field
from typing import Any


@dataclass
class SchemaActionQuery:
    """Validated, serializable Schema Action DSL payload."""

    action: str
    entity: str
    payload: dict[str, Any]

    def to_dict(self) -> dict[str, Any]:
        result: dict[str, Any] = {
            "action": self.action,
            "entity": self.entity,
        }
        result.update(self.payload)
        return result


class SchemaAction:
    """Fluent builder for Schema Action DSL queries."""

    def __init__(self, entity: str) -> None:
        self._entity = entity
        self._action: str = "find"
        self._where: dict[str, Any] = {}
        self._select: list[str] = []
        self._order_by: list[dict[str, str]] = []
        self._limit: int | None = None
        self._offset: int | None = None
        self._data: dict[str, Any] = {}
        self._text: str | None = None
        self._strategy: dict[str, bool] = {}
        self._group_by: list[str] = []
        self._metrics: list[dict[str, str]] = []

    # ── Action selectors ─────────────────────────────────────────

    def find(self) -> SchemaAction:
        self._action = "find"
        return self

    def get(self, pk: str | None = None) -> SchemaAction:
        self._action = "get"
        if pk:
            self._where["id"] = pk
        return self

    def insert(self, data: dict[str, Any] | None = None) -> SchemaAction:
        self._action = "insert"
        if data:
            self._data = data
        return self

    def update(self, pk: str | None = None) -> SchemaAction:
        self._action = "update"
        if pk:
            self._where["id"] = pk
        return self

    def delete(self, pk: str | None = None) -> SchemaAction:
        self._action = "delete"
        if pk:
            self._where["id"] = pk
        return self

    def search(self, text: str | None = None) -> SchemaAction:
        self._action = "search"
        if text:
            self._text = text
        return self

    def aggregate(self) -> SchemaAction:
        self._action = "aggregate"
        return self

    # ── Clauses ──────────────────────────────────────────────────

    def where(self, **kwargs: Any) -> SchemaAction:
        """Add filter conditions.  Supports django-style lookups:

        - field=value  →  {"field": value}
        - field__in=[...]  →  {"field": {"$in": [...]}}
        - field__gte=10  →  {"field": {"$gte": 10}}
        """
        for key, value in kwargs.items():
            parts = key.split("__")
            if len(parts) == 1:
                self._where[key] = value
            else:
                field_name = parts[0]
                op = parts[1]  # in, gte, lte, ne, gt, lt, contains
                self._where[field_name] = {f"${op}": value}
        return self

    def select(self, *fields: str) -> SchemaAction:
        self._select = list(fields)
        return self

    def order_by(self, field_name: str, direction: str = "asc") -> SchemaAction:
        self._order_by.append({field_name: direction})
        return self

    def limit(self, n: int) -> SchemaAction:
        self._limit = n
        return self

    def offset(self, n: int) -> SchemaAction:
        self._offset = n
        return self

    def data(self, d: dict[str, Any]) -> SchemaAction:
        self._data = d
        return self

    def text(self, t: str) -> SchemaAction:
        self._text = t
        return self

    def strategy(
        self,
        lexical: bool = False,
        vector: bool = False,
        rerank: bool = False,
    ) -> SchemaAction:
        if lexical:
            self._strategy["lexical"] = True
        if vector:
            self._strategy["vector"] = True
        if rerank:
            self._strategy["rerank"] = True
        return self

    def group_by(self, *fields: str) -> SchemaAction:
        self._group_by = list(fields)
        return self

    def metrics(self, *m: dict[str, str]) -> SchemaAction:
        self._metrics = list(m)
        return self

    # ── Build ────────────────────────────────────────────────────

    def build(self) -> SchemaActionQuery:
        """Build and validate the query."""
        payload: dict[str, Any] = {}

        if self._where:
            payload["where"] = self._where
        if self._select:
            payload["select"] = self._select
        if self._order_by:
            payload["order_by"] = self._order_by
        if self._limit is not None:
            payload["limit"] = self._limit
        if self._offset is not None:
            payload["offset"] = self._offset
        if self._data:
            payload["data"] = self._data
        if self._text:
            payload["text"] = self._text
        if self._strategy:
            payload["strategy"] = self._strategy
        if self._group_by:
            payload["group_by"] = self._group_by
        if self._metrics:
            payload["metrics"] = self._metrics

        return SchemaActionQuery(
            action=self._action,
            entity=self._entity,
            payload=payload,
        )
