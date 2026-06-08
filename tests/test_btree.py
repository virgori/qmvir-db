"""Tests for indexing.btree.btree — B-Tree index."""

from __future__ import annotations

import pytest

from indexing.btree.btree import BTree


@pytest.fixture
def tree() -> BTree:
    return BTree(order=4)


class TestBTree:
    def test_insert_and_get(self, tree: BTree) -> None:
        tree.insert(10, "row_10")
        assert tree.get(10) == "row_10"

    def test_get_missing(self, tree: BTree) -> None:
        assert tree.get(999) is None

    def test_insert_many(self, tree: BTree) -> None:
        for i in range(100):
            tree.insert(i, f"row_{i}")
        for i in range(100):
            assert tree.get(i) == f"row_{i}"

    def test_range_scan(self, tree: BTree) -> None:
        for i in range(20):
            tree.insert(i, f"r{i}")
        results = tree.range_scan(5, 10)
        keys = [k for k, _ in results]
        assert keys == [5, 6, 7, 8, 9, 10]

    def test_delete(self, tree: BTree) -> None:
        tree.insert(1, "a")
        tree.insert(2, "b")
        tree.delete(1)
        assert tree.get(1) is None
        assert tree.get(2) == "b"

    def test_scan_all(self, tree: BTree) -> None:
        for i in [5, 3, 1, 4, 2]:
            tree.insert(i, f"r{i}")
        items = tree.scan_all()
        keys = [k for k, _ in items]
        assert keys == [1, 2, 3, 4, 5]

    def test_unique_constraint(self) -> None:
        tree = BTree(order=4, unique=True)
        tree.insert(1, "first")
        with pytest.raises(ValueError):
            tree.insert(1, "duplicate")
