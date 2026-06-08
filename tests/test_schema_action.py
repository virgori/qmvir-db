"""Tests for sdk.schema_action_client.builder — fluent query builder."""

from __future__ import annotations

import pytest

from sdk.schema_action_client.builder import SchemaAction


class TestSchemaAction:
    def test_simple_find(self) -> None:
        q = SchemaAction("articles").find().limit(10).build()
        d = q.to_dict()
        assert d["action"] == "find"
        assert d["entity"] == "articles"
        assert d["limit"] == 10

    def test_where_simple(self) -> None:
        q = SchemaAction("articles").find().where(status="published").build()
        assert q.payload["where"]["status"] == "published"

    def test_where_operator(self) -> None:
        q = SchemaAction("articles").find().where(score__gte=4.0).build()
        assert q.payload["where"]["score"] == {"$gte": 4.0}

    def test_where_in(self) -> None:
        q = SchemaAction("articles").find().where(category__in=["tech", "ai"]).build()
        assert q.payload["where"]["category"] == {"$in": ["tech", "ai"]}

    def test_insert(self) -> None:
        q = SchemaAction("articles").insert({"title": "Hello"}).build()
        d = q.to_dict()
        assert d["action"] == "insert"
        assert d["data"]["title"] == "Hello"

    def test_search_hybrid(self) -> None:
        q = (SchemaAction("articles")
             .search("database performance")
             .strategy(lexical=True, vector=True)
             .limit(20)
             .build())
        d = q.to_dict()
        assert d["action"] == "search"
        assert d["text"] == "database performance"
        assert d["strategy"]["lexical"] is True
        assert d["strategy"]["vector"] is True

    def test_aggregate(self) -> None:
        q = (SchemaAction("events")
             .aggregate()
             .group_by("event_type")
             .metrics({"count": "*"}, {"sum": "value"})
             .build())
        d = q.to_dict()
        assert d["action"] == "aggregate"
        assert d["group_by"] == ["event_type"]
        assert len(d["metrics"]) == 2

    def test_select_and_order(self) -> None:
        q = (SchemaAction("articles")
             .find()
             .select("id", "title")
             .order_by("score", "desc")
             .build())
        d = q.to_dict()
        assert d["select"] == ["id", "title"]
        assert d["order_by"] == [{"score": "desc"}]

    def test_delete(self) -> None:
        q = SchemaAction("articles").delete("a1").build()
        d = q.to_dict()
        assert d["action"] == "delete"
        assert d["where"]["id"] == "a1"
