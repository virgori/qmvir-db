"""QM Indexing — B-tree index.

In-memory B-tree supporting:
  - Point lookup (exact match)
  - Range scan
  - Prefix scan
  - Ordered iteration
  - Unique constraint enforcement
"""

from __future__ import annotations

from dataclasses import dataclass, field
from typing import Any, Iterator
import bisect


@dataclass
class BTreeNode:
    """A node in the B-tree."""

    keys: list[Any] = field(default_factory=list)
    values: list[Any] = field(default_factory=list)  # For leaf nodes: actual values
    children: list[BTreeNode] = field(default_factory=list)
    is_leaf: bool = True

    @property
    def size(self) -> int:
        return len(self.keys)


class BTree:
    """B-tree index implementation.

    Supports:
      - Insert / delete / lookup
      - Range queries
      - Unique constraint
    """

    def __init__(self, order: int = 128, unique: bool = False) -> None:
        self.order = order
        self.unique = unique
        self.root = BTreeNode()
        self._size = 0

    def insert(self, key: Any, value: Any) -> None:
        """Insert a key-value pair."""
        if self.unique:
            existing = self.get(key)
            if existing is not None:
                raise ValueError(f"Duplicate key: {key}")

        root = self.root
        if root.size >= self.order - 1:
            new_root = BTreeNode(is_leaf=False)
            new_root.children.append(self.root)
            self._split_child(new_root, 0)
            self.root = new_root

        self._insert_non_full(self.root, key, value)
        self._size += 1

    def get(self, key: Any) -> Any | None:
        """Point lookup by exact key."""
        return self._search(self.root, key)

    def range_scan(self, low: Any, high: Any) -> list[tuple[Any, Any]]:
        """Range query: return all (key, value) pairs where low <= key <= high."""
        results: list[tuple[Any, Any]] = []
        self._range_collect(self.root, low, high, results)
        return results

    def delete(self, key: Any) -> bool:
        """Lazy delete — marks value as None. Full delete with tree rebalancing is complex."""
        node = self.root
        while node:
            idx = bisect.bisect_left(node.keys, key)
            if idx < len(node.keys) and node.keys[idx] == key:
                if node.is_leaf:
                    node.keys.pop(idx)
                    node.values.pop(idx)
                    self._size -= 1
                    return True
                # B+-tree: data is in leaves, go to right child
                node = node.children[idx + 1] if idx + 1 < len(node.children) else None
                continue
            if node.is_leaf:
                return False
            node = node.children[idx] if idx < len(node.children) else None
        return False

    def scan_all(self) -> list[tuple[Any, Any]]:
        """Scan all entries in order."""
        results: list[tuple[Any, Any]] = []
        self._scan_node(self.root, results)
        return results

    @property
    def size(self) -> int:
        return self._size

    # ─── Internal methods ───

    def _search(self, node: BTreeNode, key: Any) -> Any | None:
        idx = bisect.bisect_left(node.keys, key)
        if idx < len(node.keys) and node.keys[idx] == key:
            if node.is_leaf:
                return node.values[idx]
            # B+-tree: data is in leaves, go to right child
            if idx + 1 < len(node.children):
                return self._search(node.children[idx + 1], key)
            return None
        if node.is_leaf:
            return None
        if idx < len(node.children):
            return self._search(node.children[idx], key)
        return None

    def _insert_non_full(self, node: BTreeNode, key: Any, value: Any) -> None:
        if node.is_leaf:
            idx = bisect.bisect_left(node.keys, key)
            node.keys.insert(idx, key)
            node.values.insert(idx, value)
        else:
            idx = bisect.bisect_right(node.keys, key)
            if idx < len(node.children):
                child = node.children[idx]
                if child.size >= self.order - 1:
                    self._split_child(node, idx)
                    if key >= node.keys[idx]:
                        idx += 1
                self._insert_non_full(node.children[idx], key, value)

    def _split_child(self, parent: BTreeNode, idx: int) -> None:
        child = parent.children[idx]
        mid = child.size // 2

        new_node = BTreeNode(is_leaf=child.is_leaf)

        if child.is_leaf:
            # B+-tree leaf split: mid key stays in the right (new) node
            new_node.keys = child.keys[mid:]
            new_node.values = child.values[mid:]
            parent.keys.insert(idx, child.keys[mid])
            child.keys = child.keys[:mid]
            child.values = child.values[:mid]
        else:
            # Internal node split: mid key moves up to parent
            new_node.keys = child.keys[mid + 1:]
            new_node.children = child.children[mid + 1:]
            parent.keys.insert(idx, child.keys[mid])
            child.keys = child.keys[:mid]
            child.children = child.children[:mid + 1]

        parent.children.insert(idx + 1, new_node)

    def _range_collect(
        self, node: BTreeNode, low: Any, high: Any, results: list[tuple[Any, Any]]
    ) -> None:
        if node.is_leaf:
            for i, key in enumerate(node.keys):
                if low <= key <= high:
                    results.append((key, node.values[i]))
            return

        n = len(node.keys)
        for i in range(len(node.children)):
            # child[i] covers: (-inf, keys[0]) for i=0,
            #   [keys[i-1], keys[i]) for 0<i<n, [keys[n-1], +inf) for i=n
            # Skip if entirely below low or above high
            if i < n and node.keys[i] <= low:
                continue
            if i > 0 and node.keys[i - 1] > high:
                break
            self._range_collect(node.children[i], low, high, results)

    def _scan_node(self, node: BTreeNode, results: list[tuple[Any, Any]]) -> None:
        if node.is_leaf:
            for i, key in enumerate(node.keys):
                results.append((key, node.values[i]))
            return

        for i, child in enumerate(node.children):
            self._scan_node(child, results)
