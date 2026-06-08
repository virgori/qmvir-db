"""Tests for indexing: BTree (Python), BitmapIndex (Python), IndexManager (Rust).

Replaces old test_core.py BTree/RoaringBitmap sections + test_phases789.py index tests.
"""
from __future__ import annotations

import pytest

import qm_engine
from indexing.btree.btree import BTree
from indexing.bitmap.bitmap_index import BitmapIndex


# ─────────────────────────────────────────────────────────────────────
# BTree (Python)
# ─────────────────────────────────────────────────────────────────────

class TestBTreePython:
    @pytest.fixture
    def tree(self):
        return BTree(order=4)

    def test_insert_and_get(self, tree):
        tree.insert(10, "ten")
        assert tree.get(10) == "ten"

    def test_get_missing_returns_none(self, tree):
        assert tree.get(999) is None

    def test_multiple_inserts(self, tree):
        for i in range(50):
            tree.insert(i, f"val_{i}")
        for i in range(50):
            assert tree.get(i) == f"val_{i}"

    def test_size(self, tree):
        assert tree.size == 0
        tree.insert(1, "a")
        tree.insert(2, "b")
        assert tree.size == 2

    def test_range_scan(self, tree):
        for i in [10, 20, 30, 40, 50]:
            tree.insert(i, f"v{i}")
        result = tree.range_scan(15, 45)
        keys = [k for k, v in result]
        assert 20 in keys
        assert 30 in keys
        assert 40 in keys
        assert 10 not in keys
        assert 50 not in keys

    def test_range_scan_empty(self, tree):
        tree.insert(10, "a")
        result = tree.range_scan(20, 30)
        assert result == []

    def test_delete(self, tree):
        tree.insert(5, "five")
        assert tree.delete(5) is True
        assert tree.get(5) is None

    def test_delete_nonexistent(self, tree):
        assert tree.delete(999) is False

    def test_scan_all(self, tree):
        for i in [3, 1, 2]:
            tree.insert(i, f"v{i}")
        result = tree.scan_all()
        keys = [k for k, v in result]
        assert keys == sorted(keys)

    def test_bulk_insert(self):
        tree = BTree(order=128)
        for i in range(1000):
            tree.insert(i, i * 10)
        assert tree.size == 1000
        assert tree.get(500) == 5000

    def test_update_value(self, tree):
        tree.insert(1, "old")
        tree.insert(1, "new")
        assert tree.get(1) == "new"

    def test_unique_constraint(self):
        tree = BTree(order=4, unique=True)
        tree.insert(1, "a")
        with pytest.raises(ValueError):
            tree.insert(1, "b")

    def test_string_keys(self, tree):
        tree.insert("apple", 1)
        tree.insert("banana", 2)
        tree.insert("cherry", 3)
        assert tree.get("banana") == 2

    def test_range_scan_inclusive(self, tree):
        for i in range(1, 11):
            tree.insert(i, f"v{i}")
        result = tree.range_scan(3, 7)
        keys = [k for k, v in result]
        assert 3 in keys
        assert 7 in keys


# ─────────────────────────────────────────────────────────────────────
# BitmapIndex (Python)
# ─────────────────────────────────────────────────────────────────────

class TestBitmapIndex:
    @pytest.fixture
    def idx(self):
        return BitmapIndex(name="status_idx")

    def test_add_and_get(self, idx):
        idx.add(0, "active")
        idx.add(1, "active")
        idx.add(2, "inactive")
        result = idx.get("active")
        assert 0 in result
        assert 1 in result
        assert 2 not in result

    def test_intersect(self, idx):
        idx.add(0, "a")
        idx.add(1, "a")
        idx.add(1, "b")
        idx.add(2, "b")
        result = idx.intersect(["a", "b"])
        assert 1 in result
        assert 0 not in result

    def test_union(self, idx):
        idx.add(0, "a")
        idx.add(1, "b")
        result = idx.union(["a", "b"])
        assert 0 in result
        assert 1 in result

    def test_not_equal(self, idx):
        idx.add(0, "active")
        idx.add(1, "inactive")
        idx.add(2, "active")
        result = idx.not_equal("active")
        assert 1 in result
        assert 0 not in result

    def test_cardinality(self, idx):
        idx.add(0, "x")
        idx.add(1, "x")
        idx.add(2, "y")
        assert idx.cardinality("x") == 2
        assert idx.cardinality("y") == 1

    def test_distinct_values(self, idx):
        idx.add(0, "a")
        idx.add(1, "b")
        idx.add(2, "c")
        vals = idx.distinct_values()
        assert set(vals) == {"a", "b", "c"}

    def test_facet_counts(self, idx):
        idx.add(0, "red")
        idx.add(1, "red")
        idx.add(2, "blue")
        counts = idx.facet_counts()
        assert counts["red"] == 2
        assert counts["blue"] == 1

    def test_size(self, idx):
        assert idx.size == 0
        idx.add(0, "x")
        assert idx.size >= 1

    def test_empty_get(self, idx):
        assert idx.get("nothing") == set()


# ─────────────────────────────────────────────────────────────────────
# IndexManager (Rust) — extended tests
# ─────────────────────────────────────────────────────────────────────

class TestIndexManagerExtended:
    @pytest.fixture
    def mgr(self):
        return qm_engine.IndexManager()

    def test_evaluate_empty(self, mgr):
        decisions = mgr.evaluate()
        assert isinstance(decisions, list)

    def test_record_query_hit(self, mgr):
        mgr.create_index("idx1", "users", ["name"])
        mgr.record_query_hit("users", "name")

    def test_record_write(self, mgr):
        mgr.create_index("idx1", "users", ["id"])
        mgr.record_write("users", "id")

    def test_update_selectivity(self, mgr):
        mgr.create_index("idx1", "users", ["name"])
        mgr.update_selectivity("users", "name", 30, 100)

    def test_record_numeric_value(self, mgr):
        mgr.create_index("idx1", "users", ["age"])
        mgr.record_numeric_value("users", "age", 25.0)
        mgr.record_numeric_value("users", "age", 35.0)

    def test_histogram(self, mgr):
        mgr.create_index("idx1", "users", ["age"])
        for v in [20.0, 25.0, 30.0, 35.0, 40.0]:
            mgr.record_numeric_value("users", "age", v)
        hist = mgr.histogram("users", "age")
        assert isinstance(hist, tuple)

    def test_is_building(self, mgr):
        mgr.create_index("idx1", "users", ["id"])
        assert mgr.is_building() is False

    def test_multiple_indexes_same_table(self, mgr):
        mgr.create_index("idx1", "users", ["name"])
        mgr.create_index("idx2", "users", ["email"])
        indexes = mgr.list_indexes()
        assert len(indexes) == 2
        # Both on "users"
        assert all(idx[1] == "users" for idx in indexes)

    def test_apply_decisions(self, mgr):
        decisions = mgr.evaluate()
        mgr.apply_decisions()

    def test_selectivity_between(self, mgr):
        mgr.create_index("idx1", "users", ["age"])
        for v in range(100):
            mgr.record_numeric_value("users", "age", float(v))
        sel = mgr.update_selectivity_between("users", "age", 20.0, 40.0)
        assert isinstance(sel, float)
