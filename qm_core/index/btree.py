"""QM Index — B+Tree with leaf chain, bulk loading, and range scans.

A proper B+tree where:
    - Internal nodes store only keys + child pointers (no values)
    - Leaf nodes store (key, value) pairs + next-leaf pointer
    - Leaf chain enables efficient range scans
    - Supports: point lookup, range scan, prefix scan, min/max, count
    - Configurable order (fanout)
    - Bulk load for initial index build (sorted insert)
"""

from __future__ import annotations

import bisect
from dataclasses import dataclass, field
from typing import Any, Iterator

from qm_core.concurrency import RWLock


@dataclass
class BPlusLeaf:
    """Leaf node: stores key-value pairs + pointer to next leaf."""
    keys: list[Any] = field(default_factory=list)
    values: list[Any] = field(default_factory=list)
    next_leaf: BPlusLeaf | None = None

    @property
    def size(self) -> int:
        return len(self.keys)


@dataclass
class BPlusInternal:
    """Internal node: stores keys + child pointers."""
    keys: list[Any] = field(default_factory=list)
    children: list[Any] = field(default_factory=list)  # BPlusLeaf | BPlusInternal

    @property
    def size(self) -> int:
        return len(self.keys)


class BPlusTree:
    """B+Tree index with leaf-chain range scans.

    Args:
        order: Maximum number of children per internal node.
        unique: Enforce unique keys.
    """

    def __init__(self, order: int = 128, unique: bool = False) -> None:
        self.order = max(order, 4)
        self.unique = unique
        self._root: BPlusLeaf | BPlusInternal = BPlusLeaf()
        self._size = 0
        self._first_leaf: BPlusLeaf | None = None
        self._height = 0
        self._rwlock = RWLock()
        self._setup_first_leaf()

    # ── Public API ──────────────────────────────────────────────────

    def insert(self, key: Any, value: Any) -> None:
        """Insert a key-value pair. Thread-safe (exclusive lock)."""
        with self._rwlock.write():
            if self.unique and self._get_unlocked(key) is not None:
                raise ValueError(f"Duplicate key: {key}")
            result = self._insert(self._root, key, value)
            if result is not None:
                new_key, new_child = result
                new_root = BPlusInternal(keys=[new_key], children=[self._root, new_child])
                self._root = new_root
                self._height += 1
            self._size += 1

    def get(self, key: Any) -> Any | None:
        """Point lookup. Thread-safe (shared lock)."""
        with self._rwlock.read():
            return self._get_unlocked(key)

    def _get_unlocked(self, key: Any) -> Any | None:
        """Point lookup without lock (caller must hold lock)."""
        leaf = self._find_leaf(key)
        idx = bisect.bisect_left(leaf.keys, key)
        if idx < leaf.size and leaf.keys[idx] == key:
            return leaf.values[idx]
        return None

    def range_scan(self, low: Any, high: Any, include_low: bool = True,
                   include_high: bool = True) -> list[tuple[Any, Any]]:
        """Range scan [low, high] via leaf chain. Thread-safe (shared lock)."""
        with self._rwlock.read():
            return self._range_scan_unlocked(low, high, include_low, include_high)

    def _range_scan_unlocked(self, low: Any, high: Any, include_low: bool = True,
                             include_high: bool = True) -> list[tuple[Any, Any]]:
        results: list[tuple[Any, Any]] = []
        leaf = self._find_leaf(low)
        while leaf is not None:
            for i, k in enumerate(leaf.keys):
                if include_low and k < low:
                    continue
                if not include_low and k <= low:
                    continue
                if include_high and k > high:
                    return results
                if not include_high and k >= high:
                    return results
                results.append((k, leaf.values[i]))
            leaf = leaf.next_leaf
        return results

    def scan_all(self) -> Iterator[tuple[Any, Any]]:
        """Iterate all (key, value) pairs in sorted order via leaf chain."""
        leaf = self._first_leaf
        while leaf is not None:
            for i in range(leaf.size):
                yield leaf.keys[i], leaf.values[i]
            leaf = leaf.next_leaf

    def scan_ge(self, key: Any, limit: int = 0) -> list[tuple[Any, Any]]:
        """Scan all keys >= key. With optional limit."""
        results: list[tuple[Any, Any]] = []
        leaf = self._find_leaf(key)
        while leaf is not None:
            for i, k in enumerate(leaf.keys):
                if k >= key:
                    results.append((k, leaf.values[i]))
                    if limit and len(results) >= limit:
                        return results
            leaf = leaf.next_leaf
        return results

    def delete(self, key: Any) -> bool:
        """Delete a key. Thread-safe (exclusive lock)."""
        with self._rwlock.write():
            leaf = self._find_leaf(key)
            idx = bisect.bisect_left(leaf.keys, key)
            if idx < leaf.size and leaf.keys[idx] == key:
                leaf.keys.pop(idx)
                leaf.values.pop(idx)
                self._size -= 1
                return True
            return False

    def min(self) -> tuple[Any, Any] | None:
        leaf = self._first_leaf
        if leaf and leaf.size > 0:
            return leaf.keys[0], leaf.values[0]
        return None

    def max(self) -> tuple[Any, Any] | None:
        leaf = self._first_leaf
        while leaf and leaf.next_leaf:
            leaf = leaf.next_leaf
        if leaf and leaf.size > 0:
            return leaf.keys[-1], leaf.values[-1]
        return None

    def bulk_load(self, sorted_pairs: list[tuple[Any, Any]]) -> None:
        """Build tree from pre-sorted (key, value) pairs. Thread-safe (exclusive lock)."""
        with self._rwlock.write():
            self._bulk_load_unlocked(sorted_pairs)

    def _bulk_load_unlocked(self, sorted_pairs: list[tuple[Any, Any]]) -> None:
        if not sorted_pairs:
            return
        max_leaf = self.order - 1  # Max keys per leaf

        # Build leaves
        leaves: list[BPlusLeaf] = []
        for i in range(0, len(sorted_pairs), max_leaf):
            chunk = sorted_pairs[i:i + max_leaf]
            leaf = BPlusLeaf(
                keys=[p[0] for p in chunk],
                values=[p[1] for p in chunk],
            )
            if leaves:
                leaves[-1].next_leaf = leaf
            leaves.append(leaf)

        self._first_leaf = leaves[0] if leaves else None
        self._size = len(sorted_pairs)

        # Build internal nodes bottom-up
        current_level: list[Any] = leaves
        while len(current_level) > 1:
            next_level: list[BPlusInternal] = []
            max_children = self.order
            for i in range(0, len(current_level), max_children):
                children = current_level[i:i + max_children]
                keys: list[Any] = []
                for c in children[1:]:
                    if isinstance(c, BPlusLeaf):
                        keys.append(c.keys[0])
                    else:
                        keys.append(self._leftmost_key(c))
                node = BPlusInternal(keys=keys, children=children)
                next_level.append(node)
            current_level = next_level

        self._root = current_level[0]
        self._height = self._compute_height()

    @property
    def size(self) -> int:
        return self._size

    @property
    def height(self) -> int:
        return self._height

    # ── Statistics for planner ──────────────────────────────────────

    def estimate_range_count(self, low: Any, high: Any) -> int:
        """Estimate count between low and high (for planner)."""
        # Walk leaf chain, count
        count = 0
        leaf = self._find_leaf(low)
        while leaf is not None:
            for k in leaf.keys:
                if k < low:
                    continue
                if k > high:
                    return count
                count += 1
            leaf = leaf.next_leaf
        return count

    # ── Internal ────────────────────────────────────────────────────

    def _setup_first_leaf(self) -> None:
        if isinstance(self._root, BPlusLeaf):
            self._first_leaf = self._root

    def _find_leaf(self, key: Any) -> BPlusLeaf:
        """Navigate from root to the leaf that should contain key."""
        node = self._root
        while isinstance(node, BPlusInternal):
            idx = bisect.bisect_right(node.keys, key)
            if idx < len(node.children):
                node = node.children[idx]
            else:
                node = node.children[-1]
        return node

    def _insert(self, node: Any, key: Any, value: Any) -> tuple[Any, Any] | None:
        """Insert into subtree rooted at node. Returns split info or None."""
        if isinstance(node, BPlusLeaf):
            return self._insert_leaf(node, key, value)
        else:
            return self._insert_internal(node, key, value)

    def _insert_leaf(self, leaf: BPlusLeaf, key: Any, value: Any) -> tuple[Any, Any] | None:
        idx = bisect.bisect_left(leaf.keys, key)
        # Update existing key for non-unique index
        if not self.unique and idx < leaf.size and leaf.keys[idx] == key:
            leaf.values[idx] = value
            self._size -= 1  # Will be incremented by caller
            return None
        leaf.keys.insert(idx, key)
        leaf.values.insert(idx, value)

        if leaf.size > self.order - 1:
            return self._split_leaf(leaf)
        return None

    def _split_leaf(self, leaf: BPlusLeaf) -> tuple[Any, BPlusLeaf]:
        mid = leaf.size // 2
        new_leaf = BPlusLeaf(
            keys=leaf.keys[mid:],
            values=leaf.values[mid:],
            next_leaf=leaf.next_leaf,
        )
        leaf.keys = leaf.keys[:mid]
        leaf.values = leaf.values[:mid]
        leaf.next_leaf = new_leaf

        if self._first_leaf is None:
            self._first_leaf = leaf

        return new_leaf.keys[0], new_leaf

    def _insert_internal(self, node: BPlusInternal, key: Any, value: Any) -> tuple[Any, Any] | None:
        idx = bisect.bisect_right(node.keys, key)
        child_idx = min(idx, len(node.children) - 1)
        result = self._insert(node.children[child_idx], key, value)
        if result is None:
            return None

        new_key, new_child = result
        ins_idx = bisect.bisect_right(node.keys, new_key)
        node.keys.insert(ins_idx, new_key)
        node.children.insert(ins_idx + 1, new_child)

        if node.size > self.order - 1:
            return self._split_internal(node)
        return None

    def _split_internal(self, node: BPlusInternal) -> tuple[Any, BPlusInternal]:
        mid = node.size // 2
        promote_key = node.keys[mid]
        new_node = BPlusInternal(
            keys=node.keys[mid + 1:],
            children=node.children[mid + 1:],
        )
        node.keys = node.keys[:mid]
        node.children = node.children[:mid + 1]
        return promote_key, new_node

    def _leftmost_key(self, node: Any) -> Any:
        while isinstance(node, BPlusInternal):
            node = node.children[0]
        return node.keys[0] if node.keys else None

    def _compute_height(self) -> int:
        h = 0
        node = self._root
        while isinstance(node, BPlusInternal):
            h += 1
            node = node.children[0] if node.children else None
            if node is None:
                break
        return h
