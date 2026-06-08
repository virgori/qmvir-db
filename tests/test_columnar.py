"""Tests for analytics_platform.columnar_store — columnar engine."""

from __future__ import annotations

import pytest

from analytics_platform.columnar_store.columnar import ColumnarStore


@pytest.fixture
def store() -> ColumnarStore:
    s = ColumnarStore()
    rows = [
        {"event_type": "click", "page": "home", "value": 1.0},
        {"event_type": "click", "page": "about", "value": 2.0},
        {"event_type": "view", "page": "home", "value": 1.5},
        {"event_type": "click", "page": "home", "value": 3.0},
        {"event_type": "view", "page": "about", "value": 0.5},
    ]
    for row in rows:
        s.insert("events", row)
    return s


class TestColumnarStore:
    def test_insert_and_scan(self, store: ColumnarStore) -> None:
        rows = store.scan("events")
        assert len(rows) == 5

    def test_scan_column_pruning(self, store: ColumnarStore) -> None:
        rows = store.scan("events", columns=["event_type", "value"])
        assert all("page" not in r for r in rows)
        assert all("event_type" in r and "value" in r for r in rows)

    def test_scan_predicate_pushdown(self, store: ColumnarStore) -> None:
        rows = store.scan("events", predicates={"event_type": "click"})
        assert len(rows) == 3
        assert all(r["event_type"] == "click" for r in rows)

    def test_aggregate_count(self, store: ColumnarStore) -> None:
        result = store.aggregate(
            "events",
            group_by=["event_type"],
            metrics=[{"count": "*"}],
        )
        counts = {r["event_type"]: r["count"] for r in result}
        assert counts["click"] == 3
        assert counts["view"] == 2

    def test_aggregate_sum(self, store: ColumnarStore) -> None:
        result = store.aggregate(
            "events",
            group_by=["event_type"],
            metrics=[{"sum": "value"}],
        )
        sums = {r["event_type"]: r["sum_value"] for r in result}
        assert sums["click"] == pytest.approx(6.0)
        assert sums["view"] == pytest.approx(2.0)

    def test_aggregate_avg(self, store: ColumnarStore) -> None:
        result = store.aggregate(
            "events",
            group_by=["event_type"],
            metrics=[{"avg": "value"}],
        )
        avgs = {r["event_type"]: r["avg_value"] for r in result}
        assert avgs["click"] == pytest.approx(2.0)
        assert avgs["view"] == pytest.approx(1.0)
